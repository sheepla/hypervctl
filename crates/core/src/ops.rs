//! The operation layer in front of a backend handle: resolving targets, waiting
//! for goals, refusing redundant requests and reporting progress (design 3.3).
//!
//! The CLI and the TUI both go through this layer, so waiting, de-duplication
//! and error classification are written once, here.

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use tokio::time::{sleep, sleep_until, Instant};

use crate::availability::{availability, Availability, OpKind};
use crate::checkpoint::Checkpoint;
use crate::error::{HyperVError, RawError};
use crate::handle::BackendHandle;
use crate::hyper_v::RequestedState;
use crate::ids::{CheckpointId, SwitchId, VmId};
use crate::job::{JobRef, JobStatus, Pending};
use crate::switch::SwitchSummary;
use crate::target::{Candidate, Target, TargetKind};
use crate::vm::{VmState, VmSummary};

/// How long to wait for a mutation to reach its goal (design 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitPolicy {
    /// Return as soon as the backend accepted the request (`--no-wait`).
    NoWait,
    /// Wait until the goal is reached. `None` has no time limit (`--timeout 0`).
    Wait(Option<Duration>),
}

impl WaitPolicy {
    /// The default `--timeout` (spec 2.7).
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

    /// The deadline of an operation that starts at `started`.
    fn deadline(self, started: Instant) -> Option<Instant> {
        match self {
            Self::Wait(Some(limit)) => Some(started + limit),
            Self::NoWait | Self::Wait(None) => None,
        }
    }
}

/// How often a job is polled while waiting (design 3.3).
const JOB_POLL: Duration = Duration::from_millis(200);

/// How often the state is polled while waiting for a state (design 3.3).
const STATE_POLL: Duration = Duration::from_millis(500);

/// The minimum uptime drop that counts as a reboot (design 3.3).
const REBOOT_UPTIME_SLACK: Duration = Duration::from_secs(2);

/// Where an operation is, for spinners and progress bars (design 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Resolving,
    Requesting,
    Waiting,
    Finished,
}

/// One progress report. Progress is a callback, never a rendered line: what it
/// looks like belongs to the frontend (design 3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The target exactly as the user typed it.
    pub target: String,
    pub phase: Phase,
    /// Completion percentage when the backend reports one.
    pub percent: Option<u8>,
}

/// Which entity an operation resolved to, for the result rows (design 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpId {
    Vm(VmId),
    Checkpoint(CheckpointId),
}

/// What one target's operation came to (design 3.3).
#[derive(Debug, PartialEq)]
pub struct OpOutcome {
    /// The target exactly as the user typed it.
    pub target: String,
    /// The entity the operation acted on, once the target resolved (design 4.4).
    pub id: Option<OpId>,
    /// Set only for checkpoint create: the new checkpoint's id and name
    /// after the optional rename (design 3.3).
    pub created: Option<(CheckpointId, String)>,
    pub result: Result<OpSuccess, HyperVError>,
}

/// The success half of an [`OpOutcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpSuccess {
    /// The goal was already met, so nothing was requested (spec 2.9).
    Already { state: VmState },
    /// Accepted, not waited for (`--no-wait`).
    Submitted { pending: Pending },
    /// The goal was reached.
    Completed { state: VmState, elapsed: Duration },
}

/// A verb a command can ask for on one VM (spec 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VmOp {
    Start,
    Stop,
    StopForce,
    Restart,
    RestartForce,
    Save,
    Pause,
    Resume,
}

impl VmOp {
    pub const fn is_restart(self) -> bool {
        matches!(self, Self::Restart | Self::RestartForce)
    }

    /// The corresponding availability-table operation.
    pub const fn op_kind(self) -> OpKind {
        match self {
            Self::Start => OpKind::Start,
            Self::Stop => OpKind::Stop,
            Self::StopForce => OpKind::StopForce,
            Self::Restart => OpKind::Restart,
            Self::RestartForce => OpKind::RestartForce,
            Self::Save => OpKind::Save,
            Self::Pause => OpKind::Pause,
            Self::Resume => OpKind::Resume,
        }
    }
}

impl fmt::Display for VmOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::StopForce => "stop --force",
            Self::Restart => "restart",
            Self::RestartForce => "restart --force",
            Self::Save => "save",
            Self::Pause => "pause",
            Self::Resume => "resume",
        })
    }
}

/// `start` resumes a saved or paused VM rather than starting it (spec 2.3).
fn start_request(state: VmState) -> RequestedState {
    match state {
        VmState::Saved | VmState::Paused => RequestedState::Resume,
        _ => RequestedState::Start,
    }
}

/// The goal state of a request, for de-duplication and waiting.
fn goal_of(request: RequestedState) -> VmState {
    match request {
        RequestedState::Start | RequestedState::Resume | RequestedState::Reset => VmState::Running,
        RequestedState::ShutDown | RequestedState::TurnOff => VmState::Off,
        RequestedState::Save => VmState::Saved,
        RequestedState::Pause => VmState::Paused,
    }
}

/// The request behind a settled state for one operation, or `Already` /
/// `InvalidState` according to the availability table (design 3.3).
fn request_for(op: VmOp, id: VmId, state: VmState) -> Result<Option<RequestedState>, HyperVError> {
    match availability(op.op_kind(), state) {
        Availability::Ok => {
            let request = match op {
                VmOp::Start => start_request(state),
                VmOp::Stop => RequestedState::ShutDown,
                VmOp::StopForce => RequestedState::TurnOff,
                VmOp::Restart | VmOp::RestartForce => RequestedState::Reset,
                VmOp::Save => RequestedState::Save,
                VmOp::Pause => RequestedState::Pause,
                VmOp::Resume => RequestedState::Resume,
            };
            Ok(Some(request))
        }
        Availability::Already => Ok(None),
        Availability::Wait | Availability::Reject => Err(HyperVError::InvalidState {
            target: Some(id),
            state,
            operation: op.to_string(),
            raw: None,
        }),
    }
}

/// Per-operation state: the target as typed, the deadline, and who hears about
/// progress. One is built for each target and dropped with it.
struct Ctx<'a> {
    target: &'a str,
    wait: WaitPolicy,
    started: Instant,
    deadline: Option<Instant>,
    on_progress: &'a mut (dyn FnMut(Progress) + Send),
}

impl<'a> Ctx<'a> {
    fn new(
        target: &'a str,
        wait: WaitPolicy,
        on_progress: &'a mut (dyn FnMut(Progress) + Send),
    ) -> Self {
        let started = Instant::now();
        Self {
            target,
            wait,
            started,
            deadline: wait.deadline(started),
            on_progress,
        }
    }
}

fn notify(ctx: &mut Ctx<'_>, phase: Phase, percent: Option<u8>) {
    let progress = Progress {
        target: ctx.target.to_string(),
        phase,
        percent,
    };
    (ctx.on_progress)(progress);
}

fn timed_out(ctx: &Ctx<'_>) -> bool {
    ctx.deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
}

/// Sleeps for `interval`, never past `deadline`.
async fn sleep_for(interval: Duration, deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline.min(Instant::now() + interval)).await,
        None => sleep(interval).await,
    }
}

/// The operation layer (design 3.3). Cheap to build: it only holds a handle.
#[derive(Debug, Clone)]
pub struct Operations {
    handle: BackendHandle,
}

impl Operations {
    pub fn new(handle: BackendHandle) -> Self {
        Self { handle }
    }

    /// The handle, for the frontends that read without running anything.
    pub fn handle(&self) -> &BackendHandle {
        &self.handle
    }

    /// Resolves a GUID or an exactly matching name (spec 2.6).
    ///
    /// A GUID wins over a name of the same spelling: identifiers are never
    /// ambiguous, only names are.
    pub async fn resolve_vm(&self, spec: &str) -> Result<VmId, HyperVError> {
        if let Ok(id) = spec.parse::<VmId>() {
            return Ok(id);
        }
        Ok(self.named_vm(spec).await?.id)
    }

    /// The VM behind a GUID or an exactly matching name (spec 2.6).
    ///
    /// Prompts that have to show both the name and the id of a target ask for
    /// this (design 4.6). A GUID is looked up, so a VM that no longer exists is
    /// `NotFound` here even though [`Self::resolve_vm`] would accept it.
    pub async fn vm_summary(&self, spec: &str) -> Result<VmSummary, HyperVError> {
        if let Ok(id) = spec.parse::<VmId>() {
            return Ok(self.handle.get_vm(id).await?.summary);
        }
        self.named_vm(spec).await
    }

    /// The VM whose name matches exactly; zero hits are `NotFound`, many are
    /// `Ambiguous` with their identifiers (spec 2.6).
    async fn named_vm(&self, spec: &str) -> Result<VmSummary, HyperVError> {
        let vms = self.handle.list_vms().await?;
        let hits: Vec<&VmSummary> = vms.iter().filter(|vm| vm.name == spec).collect();
        match hits.as_slice() {
            [] => Err(HyperVError::NotFound {
                kind: TargetKind::Vm,
                specified: spec.to_string(),
                raw: None,
            }),
            [one] => Ok((*one).clone()),
            many => Err(HyperVError::Ambiguous {
                kind: TargetKind::Vm,
                specified: spec.to_string(),
                candidates: many
                    .iter()
                    .map(|vm| Candidate {
                        target: Target::Vm(vm.id),
                        name: vm.name.clone(),
                    })
                    .collect(),
                raw: None,
            }),
        }
    }

    /// Resolves a checkpoint by GUID or by exact name inside one VM (spec 2.6).
    pub async fn resolve_checkpoint(
        &self,
        vm: VmId,
        spec: &str,
    ) -> Result<CheckpointId, HyperVError> {
        if let Ok(id) = spec.parse::<CheckpointId>() {
            return Ok(id);
        }
        Ok(self.checkpoint_entry(vm, spec).await?.id)
    }

    /// The checkpoint behind a GUID or an exact name inside one VM (spec 2.6).
    ///
    /// Prompts that have to show both the name and the id of a target ask for
    /// this (design 4.6).
    pub async fn checkpoint_entry(&self, vm: VmId, spec: &str) -> Result<Checkpoint, HyperVError> {
        let by_id = spec.parse::<CheckpointId>().ok();
        let checkpoints = self.handle.list_checkpoints(vm).await?;
        let hits: Vec<&Checkpoint> = checkpoints
            .iter()
            .filter(|checkpoint| match by_id {
                Some(id) => checkpoint.id == id,
                None => checkpoint.name == spec,
            })
            .collect();
        match hits.as_slice() {
            [] => Err(HyperVError::NotFound {
                kind: TargetKind::Checkpoint,
                specified: spec.to_string(),
                raw: None,
            }),
            [one] => Ok((*one).clone()),
            many => Err(HyperVError::Ambiguous {
                kind: TargetKind::Checkpoint,
                specified: spec.to_string(),
                candidates: many
                    .iter()
                    .map(|checkpoint| Candidate {
                        target: Target::Checkpoint(checkpoint.id),
                        name: checkpoint.name.clone(),
                    })
                    .collect(),
                raw: None,
            }),
        }
    }

    /// Resolves a switch by GUID or by exact name (spec 2.6).
    pub async fn resolve_switch(&self, spec: &str) -> Result<SwitchId, HyperVError> {
        if let Ok(id) = spec.parse::<SwitchId>() {
            return Ok(id);
        }
        let switches = self.handle.list_switches().await?;
        let hits: Vec<&SwitchSummary> = switches
            .iter()
            .filter(|switch| switch.name == spec)
            .collect();
        match hits.as_slice() {
            [] => Err(HyperVError::NotFound {
                kind: TargetKind::Switch,
                specified: spec.to_string(),
                raw: None,
            }),
            [one] => Ok(one.id),
            many => Err(HyperVError::Ambiguous {
                kind: TargetKind::Switch,
                specified: spec.to_string(),
                candidates: many
                    .iter()
                    .map(|switch| Candidate {
                        target: Target::Switch(switch.id),
                        name: switch.name.clone(),
                    })
                    .collect(),
                raw: None,
            }),
        }
    }

    /// Runs one verb on every target, in order, and never stops at the first
    /// failure: the caller gets one outcome per target (spec 2.6).
    pub async fn run_vm(
        &self,
        op: VmOp,
        specs: &[String],
        wait: WaitPolicy,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Vec<OpOutcome> {
        let mut outcomes = Vec::with_capacity(specs.len());
        for spec in specs {
            let mut ctx = Ctx::new(spec, wait, on_progress);
            notify(&mut ctx, Phase::Resolving, None);
            let mut id = None;
            let result = match self.resolve_vm(spec).await {
                Err(error) => Err(error),
                Ok(found) => {
                    id = Some(OpId::Vm(found));
                    if op.is_restart() {
                        self.restart(found, op == VmOp::RestartForce, &mut ctx)
                            .await
                    } else {
                        self.change_state(found, op, &mut ctx).await
                    }
                }
            };
            notify(&mut ctx, Phase::Finished, None);
            outcomes.push(OpOutcome {
                target: spec.clone(),
                id,
                created: None,
                result,
            });
        }
        outcomes
    }

    /// Creates a checkpoint, identifies it by set difference, and optionally
    /// renames it (design 3.3).
    ///
    /// The backend's `create_checkpoint` carries no identifier, so the caller
    /// snapshots the id set before and after and takes the difference. With
    /// `--no-wait` only the request is submitted and no id is returned.
    pub async fn create_checkpoint(
        &self,
        vm: &str,
        name: Option<&str>,
        wait: WaitPolicy,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> OpOutcome {
        let mut ctx = Ctx::new(vm, wait, on_progress);
        notify(&mut ctx, Phase::Resolving, None);
        let mut created = None;
        let result = match self.resolve_vm(vm).await {
            Err(error) => Err(error),
            Ok(id) => {
                // Snapshot the id set before the request (design 3.3 step 1).
                let before: HashSet<CheckpointId> = match self.handle.list_checkpoints(id).await {
                    Ok(list) => list.into_iter().map(|cp| cp.id).collect(),
                    Err(error) => {
                        return OpOutcome {
                            target: vm.to_string(),
                            id: Some(OpId::Vm(id)),
                            created: None,
                            result: Err(error),
                        }
                    }
                };
                notify(&mut ctx, Phase::Requesting, None);
                let pending = match self.handle.create_checkpoint(id).await {
                    Ok(pending) => pending,
                    Err(error) => {
                        return OpOutcome {
                            target: vm.to_string(),
                            id: Some(OpId::Vm(id)),
                            created: None,
                            result: Err(error),
                        }
                    }
                };
                if ctx.wait == WaitPolicy::NoWait {
                    return OpOutcome {
                        target: vm.to_string(),
                        id: Some(OpId::Vm(id)),
                        created: None,
                        result: Ok(OpSuccess::Submitted { pending }),
                    };
                }
                notify(&mut ctx, Phase::Waiting, None);
                // Wait for the job to finish before reading the new set.
                let finished = match self.await_pending(id, pending, None, &mut ctx).await {
                    Ok(state) => state,
                    Err(error) => {
                        return OpOutcome {
                            target: vm.to_string(),
                            id: Some(OpId::Vm(id)),
                            created: None,
                            result: Err(error),
                        }
                    }
                };
                // Step 3: set difference.
                let after = match self.handle.list_checkpoints(id).await {
                    Ok(list) => list,
                    Err(error) => {
                        return OpOutcome {
                            target: vm.to_string(),
                            id: Some(OpId::Vm(id)),
                            created: None,
                            result: Err(error),
                        }
                    }
                };
                let new: Vec<&Checkpoint> =
                    after.iter().filter(|cp| !before.contains(&cp.id)).collect();
                let (new_id, new_name) = match new.as_slice() {
                    [one] => (one.id, one.name.clone()),
                    // Could not tell which checkpoint was created: do not
                    // guess (design 3.3 step 3).
                    _ => {
                        return OpOutcome {
                            target: vm.to_string(),
                            id: Some(OpId::Vm(id)),
                            created: None,
                            result: Err(HyperVError::backend(RawError::new(
                                "could not identify the created checkpoint",
                            ))),
                        }
                    }
                };
                // Step 4: optional rename.
                let final_name = match name {
                    None => new_name,
                    Some(wanted) => {
                        notify(&mut ctx, Phase::Requesting, None);
                        match self.handle.rename_checkpoint(new_id, wanted).await {
                            Ok(rename_pending) => {
                                match self.await_pending(id, rename_pending, None, &mut ctx).await {
                                    Ok(_) => wanted.to_string(),
                                    Err(error) => {
                                        return OpOutcome {
                                            target: vm.to_string(),
                                            id: Some(OpId::Vm(id)),
                                            created: Some((new_id, new_name)),
                                            result: Err(error),
                                        }
                                    }
                                }
                            }
                            // The checkpoint exists but the rename failed:
                            // report the id so the user can clean up
                            // (design 3.3 step 4).
                            Err(error) => {
                                return OpOutcome {
                                    target: vm.to_string(),
                                    id: Some(OpId::Vm(id)),
                                    created: Some((new_id, new_name)),
                                    result: Err(error),
                                }
                            }
                        }
                    }
                };
                created = Some((new_id, final_name.clone()));
                Ok(OpSuccess::Completed {
                    state: finished,
                    elapsed: ctx.started.elapsed(),
                })
            }
        };
        OpOutcome {
            target: vm.to_string(),
            id: None,
            created,
            result,
        }
    }

    /// Renames a checkpoint. The VM is only needed to resolve the checkpoint.
    pub async fn rename_checkpoint(
        &self,
        vm: &str,
        checkpoint: &str,
        name: &str,
        wait: WaitPolicy,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> OpOutcome {
        let mut ctx = Ctx::new(vm, wait, on_progress);
        notify(&mut ctx, Phase::Resolving, None);
        let mut id = None;
        let result = match self.resolve_vm(vm).await {
            Err(error) => Err(error),
            Ok(found) => match self.resolve_checkpoint(found, checkpoint).await {
                Err(error) => Err(error),
                Ok(target) => {
                    id = Some(OpId::Checkpoint(target));
                    notify(&mut ctx, Phase::Requesting, None);
                    match self.handle.rename_checkpoint(target, name).await {
                        Err(error) => Err(error),
                        Ok(pending) => self.finish(found, pending, None, &mut ctx).await,
                    }
                }
            },
        };
        OpOutcome {
            target: checkpoint.to_string(),
            id,
            created: None,
            result,
        }
    }

    /// Restores a checkpoint of a VM that must not be running (spec 2.4).
    pub async fn restore_checkpoint(
        &self,
        vm: &str,
        checkpoint: &str,
        wait: WaitPolicy,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> OpOutcome {
        let mut ctx = Ctx::new(vm, wait, on_progress);
        notify(&mut ctx, Phase::Resolving, None);
        let mut id = None;
        let result = match self.resolve_vm(vm).await {
            Err(error) => Err(error),
            Ok(found) => match self.resolve_checkpoint(found, checkpoint).await {
                Err(error) => Err(error),
                Ok(target) => {
                    id = Some(OpId::Checkpoint(target));
                    notify(&mut ctx, Phase::Requesting, None);
                    match self.handle.restore_checkpoint(found, target).await {
                        Err(error) => Err(error),
                        Ok(pending) => self.finish(found, pending, None, &mut ctx).await,
                    }
                }
            },
        };
        OpOutcome {
            target: checkpoint.to_string(),
            id,
            created: None,
            result,
        }
    }

    /// Removes one checkpoint; its children stay, its disks are marked for merge
    /// (design 3.1).
    pub async fn remove_checkpoint(
        &self,
        vm: &str,
        checkpoint: &str,
        wait: WaitPolicy,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> OpOutcome {
        let mut ctx = Ctx::new(vm, wait, on_progress);
        notify(&mut ctx, Phase::Resolving, None);
        let mut id = None;
        let result = match self.resolve_vm(vm).await {
            Err(error) => Err(error),
            Ok(found) => match self.resolve_checkpoint(found, checkpoint).await {
                Err(error) => Err(error),
                Ok(target) => {
                    id = Some(OpId::Checkpoint(target));
                    notify(&mut ctx, Phase::Requesting, None);
                    match self.handle.remove_checkpoint(target).await {
                        Err(error) => Err(error),
                        Ok(pending) => self.finish(found, pending, None, &mut ctx).await,
                    }
                }
            },
        };
        OpOutcome {
            target: checkpoint.to_string(),
            id,
            created: None,
            result,
        }
    }

    /// Reads the state of one VM.
    async fn state(&self, id: VmId) -> Result<VmState, HyperVError> {
        Ok(self.handle.get_vm(id).await?.summary.state)
    }

    /// Reads the uptime of one VM, if it reports one.
    async fn uptime(&self, id: VmId) -> Option<Duration> {
        self.handle
            .get_vm(id)
            .await
            .ok()
            .and_then(|vm| vm.summary.uptime)
    }

    /// Lets a transition in flight finish, so the decision below is made on a
    /// stable state. With `--no-wait` nothing is waited for and the transition
    /// is reported as it is (design 3.3).
    async fn settle(&self, id: VmId, ctx: &mut Ctx<'_>) -> Result<VmState, HyperVError> {
        let mut state = self.state(id).await?;
        while state.is_transition() {
            if ctx.wait == WaitPolicy::NoWait {
                return Ok(state);
            }
            if timed_out(ctx) {
                return Err(self.timeout_error(id, state, ctx).await);
            }
            notify(ctx, Phase::Waiting, None);
            sleep_for(STATE_POLL, ctx.deadline).await;
            state = self.state(id).await?;
        }
        Ok(state)
    }

    /// One verb on one already resolved VM.
    async fn change_state(
        &self,
        id: VmId,
        op: VmOp,
        ctx: &mut Ctx<'_>,
    ) -> Result<OpSuccess, HyperVError> {
        let observed = self.settle(id, ctx).await?;
        let request = match request_for(op, id, observed)? {
            Some(request) => request,
            None => return Ok(OpSuccess::Already { state: observed }),
        };
        let goal = goal_of(request);
        self.request_and_wait(id, request, goal, observed, ctx)
            .await
    }

    /// Restarts a VM via `InitiateReboot` (spec 2.3 note, design 3.3).
    ///
    /// The VM stays Running throughout, so completion is detected by the
    /// uptime resetting. `force` uses `Reset` instead and waits for Running.
    /// A VM that is not Running is not restarted: the availability table
    /// only allows restart from Running.
    async fn restart(
        &self,
        id: VmId,
        force: bool,
        ctx: &mut Ctx<'_>,
    ) -> Result<OpSuccess, HyperVError> {
        let observed = self.settle(id, ctx).await?;
        // `restart --force` goes through the Reset request, which the
        // availability table already gated on Running.
        if force {
            let request = match request_for(VmOp::RestartForce, id, observed)? {
                Some(request) => request,
                None => return Ok(OpSuccess::Already { state: observed }),
            };
            let goal = goal_of(request);
            return self
                .request_and_wait(id, request, goal, observed, ctx)
                .await;
        }
        // Non-force restart: only from Running (availability table).
        match availability(OpKind::Restart, observed) {
            Availability::Ok => {}
            Availability::Already => return Ok(OpSuccess::Already { state: observed }),
            Availability::Wait | Availability::Reject => {
                return Err(HyperVError::InvalidState {
                    target: Some(id),
                    state: observed,
                    operation: VmOp::Restart.to_string(),
                    raw: None,
                });
            }
        }
        // Record the uptime before the request (design 3.3).
        let uptime_before = if ctx.wait == WaitPolicy::NoWait {
            None
        } else {
            self.uptime(id).await
        };
        notify(ctx, Phase::Requesting, None);
        let pending = self.handle.request_guest_reboot(id).await?;
        match pending {
            Pending::AwaitReboot {
                uptime_before: reported,
                ..
            } => {
                let before = uptime_before.or(reported);
                if ctx.wait == WaitPolicy::NoWait {
                    return Ok(OpSuccess::Submitted {
                        pending: Pending::AwaitReboot {
                            vm: id,
                            uptime_before: before,
                        },
                    });
                }
                notify(ctx, Phase::Waiting, None);
                let state = self.await_reboot(id, before, ctx).await?;
                Ok(OpSuccess::Completed {
                    state,
                    elapsed: ctx.started.elapsed(),
                })
            }
            // A backend that answers with a job or a state follows the
            // normal path.
            other => self.finish(id, other, Some(VmState::Running), ctx).await,
        }
    }

    /// Sends one request and waits for its goal, if the policy asks for it.
    async fn request_and_wait(
        &self,
        id: VmId,
        request: RequestedState,
        goal: VmState,
        observed: VmState,
        ctx: &mut Ctx<'_>,
    ) -> Result<OpSuccess, HyperVError> {
        notify(ctx, Phase::Requesting, None);
        let pending = match self.handle.request_vm_state(id, request).await {
            Ok(pending) => pending,
            // The state changed between the read and the request. If it is now
            // the goal, the goal is met and there is nothing to report as a
            // failure (design 3.3).
            Err(HyperVError::InvalidState {
                target,
                operation,
                raw,
                ..
            }) => {
                let now = match self.handle.get_vm(id).await {
                    Ok(vm) => vm.summary.state,
                    Err(_) => observed,
                };
                if now == goal {
                    return Ok(OpSuccess::Already { state: now });
                }
                return Err(HyperVError::InvalidState {
                    target,
                    state: now,
                    operation,
                    raw,
                });
            }
            Err(error) => return Err(error),
        };
        self.finish(id, pending, Some(goal), ctx).await
    }

    /// Waits for an accepted request, or reports it as submitted.
    async fn finish(
        &self,
        id: VmId,
        pending: Pending,
        goal: Option<VmState>,
        ctx: &mut Ctx<'_>,
    ) -> Result<OpSuccess, HyperVError> {
        if ctx.wait == WaitPolicy::NoWait {
            return Ok(OpSuccess::Submitted { pending });
        }
        notify(ctx, Phase::Waiting, None);
        let state = self.await_pending(id, pending, goal, ctx).await?;
        Ok(OpSuccess::Completed {
            state,
            elapsed: ctx.started.elapsed(),
        })
    }

    /// Follows whichever shape the backend answered with (design 3.1).
    async fn await_pending(
        &self,
        id: VmId,
        pending: Pending,
        goal: Option<VmState>,
        ctx: &mut Ctx<'_>,
    ) -> Result<VmState, HyperVError> {
        match pending {
            Pending::Done => match goal {
                Some(goal) => Ok(goal),
                None => self.state(id).await,
            },
            Pending::AwaitState(target, goal) => self.await_state(target, goal, ctx).await,
            Pending::Job(job) => self.await_job(id, job, goal, ctx).await,
            Pending::AwaitReboot { uptime_before, .. } => {
                self.await_reboot(id, uptime_before, ctx).await
            }
        }
    }

    async fn await_state(
        &self,
        id: VmId,
        goal: VmState,
        ctx: &mut Ctx<'_>,
    ) -> Result<VmState, HyperVError> {
        let mut observed = self.state(id).await?;
        loop {
            if observed == goal {
                return Ok(observed);
            }
            if timed_out(ctx) {
                return Err(self.timeout_error(id, observed, ctx).await);
            }
            notify(ctx, Phase::Waiting, None);
            sleep_for(STATE_POLL, ctx.deadline).await;
            observed = self.state(id).await?;
        }
    }

    /// Waits for a guest reboot to complete, detected by the uptime resetting
    /// (design 3.3). `uptime_before` is the uptime observed just before the
    /// request; `None` means completion cannot be confirmed.
    async fn await_reboot(
        &self,
        id: VmId,
        uptime_before: Option<Duration>,
        ctx: &mut Ctx<'_>,
    ) -> Result<VmState, HyperVError> {
        let before = match uptime_before {
            Some(before) => before,
            None => {
                // Cannot confirm completion; the request was accepted.
                return Ok(VmState::Running);
            }
        };
        loop {
            if timed_out(ctx) {
                return Err(self.timeout_error(id, VmState::Running, ctx).await);
            }
            notify(ctx, Phase::Waiting, None);
            sleep_for(STATE_POLL, ctx.deadline).await;
            // The elapsed time since the request started bounds the
            // expected uptime if no reboot happened.
            let elapsed = ctx.started.elapsed();
            if let Some(current) = self.uptime(id).await {
                // A reboot resets the uptime; anything below the old value
                // plus elapsed minus slack means the guest rebooted.
                if current + REBOOT_UPTIME_SLACK < before + elapsed {
                    return Ok(VmState::Running);
                }
            }
        }
    }

    async fn await_job(
        &self,
        id: VmId,
        job: JobRef,
        goal: Option<VmState>,
        ctx: &mut Ctx<'_>,
    ) -> Result<VmState, HyperVError> {
        let fallback = self.state(id).await?;
        loop {
            if timed_out(ctx) {
                // Waiting stops; the job itself is left alone and may still
                // finish later (design 3.3).
                return Err(self.timeout_error(id, fallback, ctx).await);
            }
            match self.handle.job_status(job.clone()).await? {
                JobStatus::Running(percent) => {
                    notify(
                        ctx,
                        Phase::Waiting,
                        percent.and_then(|p| u8::try_from(p).ok()),
                    );
                    sleep_for(JOB_POLL, ctx.deadline).await;
                }
                JobStatus::Succeeded => break,
                JobStatus::Failed { code, message } => {
                    return Err(HyperVError::JobFailed {
                        code,
                        message,
                        reason: crate::error::JobFailReason::Other,
                        raw: None,
                    });
                }
                JobStatus::Cancelled => {
                    return Err(HyperVError::backend(RawError::new(format!(
                        "job '{job}' was cancelled"
                    ))));
                }
            }
        }
        match goal {
            Some(goal) => self.await_state(id, goal, ctx).await,
            None => self.state(id).await,
        }
    }

    /// The timeout error, with the state the backend reports right now so the
    /// user sees what was still going on (design 3.3).
    async fn timeout_error(&self, id: VmId, fallback: VmState, ctx: &Ctx<'_>) -> HyperVError {
        let last_state = match self.handle.get_vm(id).await {
            Ok(vm) => vm.summary.state,
            Err(_) => fallback,
        };
        HyperVError::Timeout {
            elapsed: ctx.started.elapsed(),
            last_state,
            raw: None,
        }
    }
}
