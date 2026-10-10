//! In-memory Hyper-V stand-in: deterministic fixtures, a state machine that
//! passes through transitional states, and failure injection (design 3.5).
//!
//! The frontends never use it directly; they go through `BackendHandle` like
//! they do with the real backend. Tests may also drive it directly, which is
//! how `tick()` and the injections are meant to be used.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use hypervctl_core::vhd::vhd_path_eq;
use hypervctl_core::{
    AttachedVhd, ChangeEvent, Checkpoint, CheckpointId, CheckpointKind, HyperV, HyperVError,
    JobRef, JobStatus, Pending, RawError, RequestedState, SwitchDetail, SwitchId, SwitchSummary,
    TargetKind, VhdAttachment, VhdInfo, VmDetail, VmId, VmState, VmSummary,
};
use tokio::sync::mpsc::{self, UnboundedSender};
// A paused test clock is honoured when the mock runs on a runtime thread;
// on a plain thread this falls back to the system clock.
use tokio::time::Instant;
use uuid::Uuid;

mod fixture;

/// How transitions advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickMode {
    /// Nothing moves until [`MockBackend::tick`] is called. For tests.
    Manual,
    /// A transition finishes once its delay has elapsed, on the next call.
    /// For a running TUI without a real Hyper-V.
    Automatic,
}

#[derive(Debug)]
pub(crate) struct Transition {
    pub target: VmState,
    pub due: Instant,
}

#[derive(Debug)]
pub(crate) struct CheckpointEntry {
    pub vm: VmId,
    pub data: Checkpoint,
}

/// Default time a transition stays in its intermediate state (automatic mode).
const DEFAULT_TRANSITION_DELAY: Duration = Duration::from_millis(200);

/// In-memory backend.
#[derive(Debug)]
pub struct MockBackend {
    vms: Vec<VmDetail>,
    checkpoints: Vec<CheckpointEntry>,
    switches: Vec<SwitchDetail>,
    vhds: Vec<VhdInfo>,
    attachments: Vec<VhdAttachment>,
    transitions: HashMap<VmId, Transition>,
    jobs: HashMap<JobRef, VmId>,
    next_job: u64,
    next_checkpoint: u64,
    change_listeners: Vec<UnboundedSender<ChangeEvent>>,
    job_hold_polls: Option<u32>,
    job_polls: HashMap<JobRef, u32>,
    tick_mode: TickMode,
    transition_delay: Duration,
    injected_error: Option<HyperVError>,
    injected_delay: Option<Duration>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    /// An empty host: no VMs, automatic ticks.
    pub fn new() -> Self {
        Self {
            vms: Vec::new(),
            checkpoints: Vec::new(),
            switches: Vec::new(),
            vhds: Vec::new(),
            attachments: Vec::new(),
            transitions: HashMap::new(),
            jobs: HashMap::new(),
            next_job: 0,
            next_checkpoint: 0,
            change_listeners: Vec::new(),
            job_hold_polls: None,
            job_polls: HashMap::new(),
            tick_mode: TickMode::Automatic,
            transition_delay: DEFAULT_TRANSITION_DELAY,
            injected_error: None,
            injected_delay: None,
        }
    }

    pub fn with_tick_mode(mode: TickMode) -> Self {
        let mut backend = Self::new();
        backend.tick_mode = mode;
        backend
    }

    /// Advances every pending transition by one step, in any tick mode.
    pub fn tick(&mut self) {
        let pending: Vec<VmId> = self.transitions.keys().copied().collect();
        for id in pending {
            self.settle(id);
        }
    }

    /// Makes the next call fail with `error`.
    pub fn inject_error(&mut self, error: HyperVError) {
        self.injected_error = Some(error);
    }

    /// Makes the next call block for `delay`.
    pub fn inject_delay(&mut self, delay: Duration) {
        self.injected_delay = Some(delay);
    }

    /// Keeps every job Running for its first polls queries, whatever the
    /// clock says, and finished after that. Tests use it to reach a timeout and
    /// then watch the very same job finish, without waiting (design 3.5).
    pub fn hold_jobs_for_polls(&mut self, polls: u32) {
        self.job_hold_polls = Some(polls);
    }

    pub fn set_transition_delay(&mut self, delay: Duration) {
        self.transition_delay = delay;
    }

    /// Consumes pending injections and, in automatic mode, settles due
    /// transitions. Every trait method starts with this.
    fn begin(&mut self) -> Result<(), HyperVError> {
        if let Some(delay) = self.injected_delay.take() {
            thread::sleep(delay);
        }
        if let Some(error) = self.injected_error.take() {
            return Err(error);
        }
        self.advance();
        Ok(())
    }

    fn advance(&mut self) {
        if self.tick_mode != TickMode::Automatic {
            return;
        }
        let now = Instant::now();
        let due: Vec<VmId> = self
            .transitions
            .iter()
            .filter(|(_, transition)| transition.due <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            self.settle(id);
        }
    }

    fn settle(&mut self, id: VmId) {
        if let Some(transition) = self.transitions.remove(&id) {
            if let Ok(vm) = self.vm_mut(id) {
                vm.summary.state = transition.target;
            }
            self.announce(ChangeEvent::Vms);
        }
    }

    /// Tells everyone who subscribed that something changed (design 3.4). A
    /// listener whose channel closed is dropped instead of being written to.
    fn announce(&mut self, event: ChangeEvent) {
        self.change_listeners
            .retain(|listener| listener.send(event.clone()).is_ok());
    }

    fn vm_mut(&mut self, id: VmId) -> Result<&mut VmDetail, HyperVError> {
        self.vms
            .iter_mut()
            .find(|vm| vm.summary.id == id)
            .ok_or_else(|| HyperVError::NotFound {
                kind: TargetKind::Vm,
                specified: id.to_string(),
                raw: None,
            })
    }

    fn new_job(&mut self, vm: VmId) -> JobRef {
        self.next_job += 1;
        let job = JobRef::new(format!("mock-job-{}", self.next_job));
        self.jobs.insert(job.clone(), vm);
        job
    }

    fn next_checkpoint_id(&mut self) -> CheckpointId {
        self.next_checkpoint += 1;
        CheckpointId::new(Uuid::from_u128(u128::from(
            0x00C0_0000_0000_0000_u64 + self.next_checkpoint,
        )))
    }
}

impl HyperV for MockBackend {
    fn list_vms(&mut self) -> Result<Vec<VmSummary>, HyperVError> {
        self.begin()?;
        Ok(self.vms.iter().map(|vm| vm.summary.clone()).collect())
    }

    fn get_vm(&mut self, id: VmId) -> Result<VmDetail, HyperVError> {
        self.begin()?;
        self.vms
            .iter()
            .find(|vm| vm.summary.id == id)
            .cloned()
            .ok_or_else(|| HyperVError::NotFound {
                kind: TargetKind::Vm,
                specified: id.to_string(),
                raw: None,
            })
    }

    fn list_checkpoints(&mut self, vm: VmId) -> Result<Vec<Checkpoint>, HyperVError> {
        self.begin()?;
        self.vm_mut(vm)?;
        Ok(self
            .checkpoints
            .iter()
            .filter(|entry| entry.vm == vm)
            .map(|entry| entry.data.clone())
            .collect())
    }

    fn list_switches(&mut self) -> Result<Vec<SwitchSummary>, HyperVError> {
        self.begin()?;
        Ok(self.switches.iter().map(|sw| sw.summary.clone()).collect())
    }

    fn get_switch(&mut self, id: SwitchId) -> Result<SwitchDetail, HyperVError> {
        self.begin()?;
        self.switches
            .iter()
            .find(|sw| sw.summary.id == id)
            .cloned()
            .ok_or_else(|| HyperVError::NotFound {
                kind: TargetKind::Switch,
                specified: id.to_string(),
                raw: None,
            })
    }

    fn list_vhds(&mut self, vm: Option<VmId>) -> Result<Vec<AttachedVhd>, HyperVError> {
        self.begin()?;
        Ok(self
            .attachments
            .iter()
            .filter(|attachment| vm.is_none_or(|id| attachment.vm_id == id))
            .map(|attachment| {
                let info = self
                    .vhds
                    .iter()
                    .find(|vhd| {
                        vhd_path_eq(
                            vhd.path.to_string_lossy().as_ref(),
                            attachment.path.to_string_lossy().as_ref(),
                        )
                    })
                    .cloned();
                AttachedVhd {
                    attachment: attachment.clone(),
                    info,
                }
            })
            .collect())
    }

    fn get_vhd(&mut self, path: &str) -> Result<VhdInfo, HyperVError> {
        self.begin()?;
        self.vhds
            .iter()
            .find(|vhd| vhd_path_eq(vhd.path.to_string_lossy().as_ref(), path))
            .cloned()
            .ok_or_else(|| HyperVError::NotFound {
                kind: TargetKind::Vhd,
                specified: path.to_string(),
                raw: None,
            })
    }

    fn request_vm_state(
        &mut self,
        id: VmId,
        request: RequestedState,
    ) -> Result<Pending, HyperVError> {
        self.begin()?;
        let (intermediate, target) = match request {
            RequestedState::Start => (VmState::Starting, VmState::Running),
            RequestedState::ShutDown => (VmState::Stopping, VmState::Off),
            RequestedState::TurnOff => (VmState::Stopping, VmState::Off),
            RequestedState::Reset => (VmState::Resetting, VmState::Running),
            RequestedState::Save => (VmState::Saving, VmState::Saved),
            RequestedState::Pause => (VmState::Pausing, VmState::Paused),
            RequestedState::Resume => (VmState::Resuming, VmState::Running),
        };
        self.vm_mut(id)?.summary.state = intermediate;
        self.transitions.insert(
            id,
            Transition {
                target,
                due: Instant::now() + self.transition_delay,
            },
        );
        self.announce(ChangeEvent::Vms);
        if request == RequestedState::ShutDown {
            // No job exists for a guest shutdown; the state is the progress.
            return Ok(Pending::AwaitState(id, target));
        }
        Ok(Pending::Job(self.new_job(id)))
    }

    fn create_checkpoint(&mut self, vm: VmId, name: Option<&str>) -> Result<Pending, HyperVError> {
        self.begin()?;
        self.vm_mut(vm)?;
        let id = self.next_checkpoint_id();
        let name = name
            .map(str::to_string)
            .unwrap_or_else(|| format!("Checkpoint {}", self.next_checkpoint));
        let parent = self
            .checkpoints
            .iter()
            .rfind(|entry| entry.vm == vm)
            .map(|entry| entry.data.id);
        self.checkpoints.push(CheckpointEntry {
            vm,
            data: Checkpoint {
                id,
                name,
                kind: CheckpointKind::Standard,
                created: Some(std::time::SystemTime::now()),
                parent_id: parent,
            },
        });
        if let Ok(vm) = self.vm_mut(vm) {
            vm.checkpoint_count += 1;
        }
        self.announce(ChangeEvent::Checkpoints(vm));
        Ok(Pending::Job(self.new_job(vm)))
    }

    fn restore_checkpoint(
        &mut self,
        vm: VmId,
        checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError> {
        self.begin()?;
        let known = self
            .checkpoints
            .iter()
            .any(|entry| entry.data.id == checkpoint);
        if !known {
            return Err(HyperVError::NotFound {
                kind: TargetKind::Checkpoint,
                specified: checkpoint.to_string(),
                raw: None,
            });
        }
        let state = self.vm_mut(vm)?.summary.state;
        if state == VmState::Running {
            // What Hyper-V answered on real hardware (M0 findings, item 6).
            return Err(HyperVError::InvalidState {
                target: Some(vm),
                state,
                operation: "restore_checkpoint".to_string(),
                raw: None,
            });
        }
        self.announce(ChangeEvent::Checkpoints(vm));
        Ok(Pending::Job(self.new_job(vm)))
    }

    fn remove_checkpoint(&mut self, checkpoint: CheckpointId) -> Result<Pending, HyperVError> {
        self.begin()?;
        let position = self
            .checkpoints
            .iter()
            .position(|entry| entry.data.id == checkpoint)
            .ok_or_else(|| HyperVError::NotFound {
                kind: TargetKind::Checkpoint,
                specified: checkpoint.to_string(),
                raw: None,
            })?;
        let entry = self.checkpoints.remove(position);
        if let Ok(vm) = self.vm_mut(entry.vm) {
            vm.checkpoint_count = vm.checkpoint_count.saturating_sub(1);
        }
        self.announce(ChangeEvent::Checkpoints(entry.vm));
        Ok(Pending::Job(self.new_job(entry.vm)))
    }

    fn job_status(&mut self, job: JobRef) -> Result<JobStatus, HyperVError> {
        self.begin()?;
        let vm =
            self.jobs.get(&job).copied().ok_or_else(|| {
                HyperVError::backend(RawError::new(format!("unknown job '{job}'")))
            })?;
        if let Some(hold) = self.job_hold_polls {
            let seen = self.job_polls.entry(job).or_insert(0);
            if *seen < hold {
                *seen += 1;
                return Ok(JobStatus::Running(None));
            }
            return Ok(JobStatus::Succeeded);
        }
        if self.transitions.contains_key(&vm) {
            Ok(JobStatus::Running(None))
        } else {
            Ok(JobStatus::Succeeded)
        }
    }

    fn subscribe_changes(&mut self) -> Result<mpsc::UnboundedReceiver<ChangeEvent>, HyperVError> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.change_listeners.push(tx);
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use hypervctl_core::VhdFormat;

    use super::*;

    fn vm_id(n: u128) -> VmId {
        crate::fixture::id(n)
    }

    /// Reports a failure with the error instead of `unwrap`/`expect`.
    fn ok<T>(result: Result<T, HyperVError>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("expected success, got {error}"),
        }
    }

    fn err<T>(result: Result<T, HyperVError>) -> HyperVError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("expected an error"),
        }
    }

    fn some<T>(value: Option<T>, what: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("expected {what}"),
        }
    }

    fn job_of(pending: Pending) -> JobRef {
        match pending {
            Pending::Job(job) => job,
            other => panic!("expected a job, got {other:?}"),
        }
    }

    fn state_of(backend: &mut MockBackend, name: &str) -> VmState {
        let listed = ok(backend.list_vms());
        let found = listed.iter().find(|vm| vm.name == name).map(|vm| vm.state);
        some(found, &format!("a VM named {name}"))
    }

    #[test]
    fn the_fixture_covers_several_states_including_an_unknown_one() {
        let mut backend = MockBackend::with_fixture();
        let vms = ok(backend.list_vms());
        assert_eq!(vms.len(), 6);
        assert!(vms.iter().any(|vm| vm.state == VmState::Unknown(42)));
        assert_eq!(vms.iter().filter(|vm| vm.name == "dup").count(), 2);
        assert!(vms.iter().any(|vm| vm.state == VmState::Saved));
        // An Off VM reports no CPU usage and no uptime (spec 2.10).
        let off = vms.iter().find(|vm| vm.name == "web-02");
        assert!(off.is_some(), "the fixture must contain web-02");
        assert_eq!(off.map(|vm| vm.cpu_usage_percent), Some(None));
        assert_eq!(off.map(|vm| vm.uptime), Some(None));
    }

    #[test]
    fn a_request_lands_in_a_transitional_state_until_it_is_ticked() {
        let mut backend = MockBackend::with_fixture();
        backend.tick_mode = TickMode::Manual;
        let running = vm_id(1);

        let pending = ok(backend.request_vm_state(running, RequestedState::TurnOff));
        assert!(matches!(pending, Pending::Job(_)));
        assert_eq!(state_of(&mut backend, "web-01"), VmState::Stopping);

        let job = job_of(pending);
        assert_eq!(
            ok(backend.job_status(job.clone())),
            JobStatus::Running(None)
        );

        backend.tick();
        assert_eq!(state_of(&mut backend, "web-01"), VmState::Off);
        assert_eq!(ok(backend.job_status(job)), JobStatus::Succeeded);
    }

    #[test]
    fn automatic_mode_settles_once_the_delay_has_elapsed() {
        let mut backend = MockBackend::with_fixture();
        backend.set_transition_delay(Duration::ZERO);

        let pending = ok(backend.request_vm_state(vm_id(2), RequestedState::Start));
        assert!(matches!(pending, Pending::Job(_)));

        // The next call runs `begin`, which settles the due transition.
        assert_eq!(state_of(&mut backend, "web-02"), VmState::Running);
    }

    #[test]
    fn a_guest_shutdown_waits_for_the_state_instead_of_a_job() {
        let mut backend = MockBackend::with_fixture();
        backend.set_transition_delay(Duration::ZERO);
        let pending = ok(backend.request_vm_state(vm_id(1), RequestedState::ShutDown));
        assert!(matches!(pending, Pending::AwaitState(_, VmState::Off)));
        assert_eq!(state_of(&mut backend, "web-01"), VmState::Off);
    }

    #[test]
    fn errors_and_delays_can_be_injected_once() {
        let mut backend = MockBackend::with_fixture();
        backend.inject_error(HyperVError::PermissionDenied { raw: None });
        assert_eq!(
            backend.list_vms(),
            Err(HyperVError::PermissionDenied { raw: None })
        );
        // The injection is consumed: the next call works again.
        assert_eq!(ok(backend.list_vms()).len(), 6);

        backend.inject_delay(Duration::from_millis(50));
        let started = Instant::now();
        let _ = ok(backend.list_vms());
        assert!(started.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn hyper_v_unavailable_can_be_injected() {
        let mut backend = MockBackend::new();
        let expected = HyperVError::HyperVUnavailable {
            reason: hypervctl_core::UnavailableReason::ServiceStopped,
            raw: None,
        };
        backend.inject_error(HyperVError::HyperVUnavailable {
            reason: hypervctl_core::UnavailableReason::ServiceStopped,
            raw: None,
        });
        assert_eq!(backend.list_vms(), Err(expected));
    }

    #[test]
    fn checkpoints_form_a_tree_and_can_be_removed() {
        let mut backend = MockBackend::with_fixture();
        let listed = ok(backend.list_checkpoints(vm_id(1)));
        assert_eq!(listed.len(), 2);
        let root_id = listed.first().map(|checkpoint| checkpoint.id);
        assert!(root_id.is_some(), "the fixture must ship a root checkpoint");
        let child_parent = listed.get(1).and_then(|checkpoint| checkpoint.parent_id);
        assert_eq!(child_parent, root_id);

        let job = job_of(ok(backend.create_checkpoint(vm_id(2), Some("manual"))));
        assert_eq!(ok(backend.job_status(job)), JobStatus::Succeeded);
        let created = ok(backend.list_checkpoints(vm_id(2)));
        assert_eq!(created.len(), 1);
        let created_id = some(
            created.first().map(|checkpoint| checkpoint.id),
            "the new checkpoint",
        );
        assert_eq!(
            created.first().map(|checkpoint| checkpoint.name.as_str()),
            Some("manual")
        );
        assert_eq!(
            created.first().and_then(|checkpoint| checkpoint.parent_id),
            None
        );
        assert_eq!(ok(backend.get_vm(vm_id(2))).checkpoint_count, 1);

        ok(backend.remove_checkpoint(created_id));
        assert!(ok(backend.list_checkpoints(vm_id(2))).is_empty());
        assert_eq!(ok(backend.get_vm(vm_id(2))).checkpoint_count, 0);
    }

    #[test]
    fn restoring_on_a_running_vm_is_rejected_the_way_hyper_v_does() {
        let mut backend = MockBackend::with_fixture();
        let error = err(backend.restore_checkpoint(vm_id(1), Uuid::from_u128(0x100 << 96).into()));
        assert!(matches!(
            error,
            HyperVError::InvalidState {
                state: VmState::Running,
                ..
            }
        ));
    }

    #[test]
    fn vhd_lookup_ignores_the_spelling_of_the_path() {
        let mut backend = MockBackend::with_fixture();
        let found = ok(backend.get_vhd(r"\\?\c:\hyperv\web-01\DISK.vhdx"));
        assert_eq!(found.format, VhdFormat::Vhdx);
        assert_eq!(
            backend.get_vhd(r"C:\other.vhdx"),
            Err(HyperVError::NotFound {
                kind: TargetKind::Vhd,
                specified: r"C:\other.vhdx".to_string(),
                raw: None,
            })
        );
    }

    #[test]
    fn an_unknown_job_is_a_backend_error() {
        let mut backend = MockBackend::with_fixture();
        assert!(matches!(
            err(backend.job_status(JobRef::from("nope"))),
            HyperVError::Backend { .. }
        ));
    }

    #[test]
    fn missing_targets_are_not_found() {
        let mut backend = MockBackend::with_fixture();
        assert_eq!(
            backend.get_vm(vm_id(999)),
            Err(HyperVError::NotFound {
                kind: TargetKind::Vm,
                specified: vm_id(999).to_string(),
                raw: None,
            })
        );
    }
}
