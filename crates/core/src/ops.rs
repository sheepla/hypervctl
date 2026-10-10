//! The operation layer in front of a backend handle: resolving targets, waiting
//! for goals, refusing redundant requests and reporting progress (design 3.3).
//!
//! The CLI and the TUI both go through this layer, so waiting, de-duplication
//! and error classification are written once, here.

use std::fmt;
use std::time::Duration;

use tokio::time::{sleep, sleep_until, Instant};

use crate::error::{HyperVError, RawError};
use crate::handle::BackendHandle;
use crate::hyper_v::RequestedState;
use crate::ids::{CheckpointId, SwitchId, VmId};
use crate::job::{JobRef, JobStatus, Pending};
use crate::switch::SwitchSummary;
use crate::target::{Candidate, Target, TargetKind};
use crate::vm::VmState;

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

/// What one target's operation came to (design 3.3).
#[derive(Debug, PartialEq)]
pub struct OpOutcome {
    pub target: String,
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

/// What to do in a state that has settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// The goal is already met: report success without touching the backend.
    Already,
    /// Send this request and watch for this state.
    Request(RequestedState, VmState),
}

/// The state a transitional state is heading towards. Hyper-V's transitional
/// values each point at one stable state, which is enough to answer a request
/// that arrives while a transition is running (design 3.7.1).
fn direction(state: VmState) -> Option<VmState> {
    use VmState::*;
    match state {
        Starting | Resuming | Resetting => Some(Running),
        Stopping => Some(Off),
        Saving => Some(Saved),
        Pausing => Some(Paused),
        _ => None,
    }
}

/// `start` resumes a saved or paused VM rather than starting it (spec 2.3).
fn start_request(state: VmState) -> RequestedState {
    match state {
        VmState::Saved | VmState::Paused => RequestedState::Resume,
        _ => RequestedState::Start,
    }
}

/// Decides between "nothing to do", "send this" and "not possible here"
/// (spec 2.9). Restart never reaches this function: it runs its own sequence.
fn plan(op: VmOp, id: VmId, state: VmState) -> Result<Plan, HyperVError> {
    use RequestedState::*;
    use VmState::*;

    let (goal, mut request, valid_from): (VmState, RequestedState, &[VmState]) = match op {
        VmOp::Start => (Running, Start, &[Off, Saved, Paused]),
        VmOp::Stop => (Off, ShutDown, &[Running]),
        VmOp::StopForce => (Off, TurnOff, &[Running, Saved, Paused]),
        VmOp::Save => (Saved, Save, &[Running, Paused]),
        VmOp::Pause => (Paused, Pause, &[Running]),
        VmOp::Resume => (Running, Resume, &[Paused]),
        // Restart runs through `restart` instead; the arm keeps `plan` total.
        VmOp::Restart | VmOp::RestartForce => (Running, Start, &[Off, Saved, Paused]),
    };

    if state == goal {
        return Ok(Plan::Already);
    }
    if !valid_from.contains(&state) {
        return Err(HyperVError::InvalidState {
            target: Some(id),
            state,
            operation: op.to_string(),
            raw: None,
        });
    }
    if op == VmOp::Start {
        request = start_request(state);
    }
    Ok(Plan::Request(request, goal))
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
        let vms = self.handle.list_vms().await?;
        let hits: Vec<&crate::vm::VmSummary> = vms.iter().filter(|vm| vm.name == spec).collect();
        match hits.as_slice() {
            [] => Err(HyperVError::NotFound {
                kind: TargetKind::Vm,
                specified: spec.to_string(),
                raw: None,
            }),
            [one] => Ok(one.id),
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
        let checkpoints = self.handle.list_checkpoints(vm).await?;
        let hits: Vec<&crate::checkpoint::Checkpoint> = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.name == spec)
            .collect();
        match hits.as_slice() {
            [] => Err(HyperVError::NotFound {
                kind: TargetKind::Checkpoint,
                specified: spec.to_string(),
                raw: None,
            }),
            [one] => Ok(one.id),
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
            let result = match self.resolve_vm(spec).await {
                Err(error) => Err(error),
                Ok(id) if op.is_restart() => {
                    self.restart(id, op == VmOp::RestartForce, &mut ctx).await
                }
                Ok(id) => self.change_state(id, op, &mut ctx).await,
            };
            notify(&mut ctx, Phase::Finished, None);
            outcomes.push(OpOutcome {
                target: spec.clone(),
                result,
            });
        }
        outcomes
    }

    /// Creates a checkpoint and waits for it when the policy says so.
    pub async fn create_checkpoint(
        &self,
        vm: &str,
        name: Option<&str>,
        wait: WaitPolicy,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<OpSuccess, HyperVError> {
        let mut ctx = Ctx::new(vm, wait, on_progress);
        notify(&mut ctx, Phase::Resolving, None);
        let id = self.resolve_vm(vm).await?;
        notify(&mut ctx, Phase::Requesting, None);
        let pending = self.handle.create_checkpoint(id, name).await?;
        self.finish(id, pending, None, &mut ctx).await
    }

    /// Restores a checkpoint of a VM that must not be running (M0 item 6).
    pub async fn restore_checkpoint(
        &self,
        vm: &str,
        checkpoint: &str,
        wait: WaitPolicy,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<OpSuccess, HyperVError> {
        let mut ctx = Ctx::new(vm, wait, on_progress);
        notify(&mut ctx, Phase::Resolving, None);
        let id = self.resolve_vm(vm).await?;
        let target = self.resolve_checkpoint(id, checkpoint).await?;
        notify(&mut ctx, Phase::Requesting, None);
        let pending = self.handle.restore_checkpoint(id, target).await?;
        self.finish(id, pending, None, &mut ctx).await
    }

    /// Removes one checkpoint; its children stay, its disks are marked for merge
    /// (design 3.1).
    pub async fn remove_checkpoint(
        &self,
        vm: &str,
        checkpoint: &str,
        wait: WaitPolicy,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<OpSuccess, HyperVError> {
        let mut ctx = Ctx::new(vm, wait, on_progress);
        notify(&mut ctx, Phase::Resolving, None);
        let id = self.resolve_vm(vm).await?;
        let target = self.resolve_checkpoint(id, checkpoint).await?;
        notify(&mut ctx, Phase::Requesting, None);
        let pending = self.handle.remove_checkpoint(target).await?;
        self.finish(id, pending, None, &mut ctx).await
    }

    /// Reads the state of one VM.
    async fn state(&self, id: VmId) -> Result<VmState, HyperVError> {
        Ok(self.handle.get_vm(id).await?.summary.state)
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
        // While not waiting, a transition still says where the VM is going, and
        // that is what the request has to be judged against.
        let effective = direction(observed).unwrap_or(observed);
        let (request, goal) = match plan(op, id, effective)? {
            Plan::Already => return Ok(OpSuccess::Already { state: observed }),
            Plan::Request(request, goal) => (request, goal),
        };
        self.request_and_wait(id, request, goal, observed, ctx)
            .await
    }

    /// Restarts a VM: shut down, confirm it stopped, start it again. The whole
    /// sequence shares one deadline (spec 2.3 note, design 3.3).
    ///
    /// `force` replaces the sequence with a single reset. A VM that is not
    /// running is only started: there is nothing to shut down first.
    async fn restart(
        &self,
        id: VmId,
        force: bool,
        ctx: &mut Ctx<'_>,
    ) -> Result<OpSuccess, HyperVError> {
        let observed = self.settle(id, ctx).await?;
        let effective = direction(observed).unwrap_or(observed);

        if effective == VmState::Running && !force {
            let outcome = self
                .request_and_wait(id, RequestedState::ShutDown, VmState::Off, observed, ctx)
                .await?;
            if matches!(outcome, OpSuccess::Submitted { .. }) {
                // `--no-wait` submits the first step and stops there.
                return Ok(outcome);
            }
        }

        if force && effective == VmState::Running {
            self.request_and_wait(id, RequestedState::Reset, VmState::Running, observed, ctx)
                .await
        } else {
            let request = start_request(effective);
            self.request_and_wait(id, request, VmState::Running, effective, ctx)
                .await
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
