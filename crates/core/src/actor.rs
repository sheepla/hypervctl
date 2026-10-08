//! The backend actor: one thread, one backend value, sequential commands.
//!
//! WMI objects are not `Send`, so the backend is *created* on this thread rather
//! than moved onto it (design 3.2): the factory is the only thing that crosses
//! the thread boundary.

use std::thread;

use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::checkpoint::{Checkpoint, CheckpointId};
use crate::error::{HyperVError, RawError};
use crate::handle::BackendHandle;
use crate::hyper_v::{HyperV, RequestedState};
use crate::ids::{SwitchId, VmId};
use crate::job::{JobRef, JobStatus, Pending};
use crate::switch::{SwitchDetail, SwitchSummary};
use crate::vhd::{VhdAttachment, VhdInfo};
use crate::vm::{VmDetail, VmSummary};

/// Bound on queued commands. The frontends never queue this many at once.
const ACTOR_CHANNEL_CAPACITY: usize = 32;

pub(crate) type Reply<T> = oneshot::Sender<Result<T, HyperVError>>;

/// One command: what to call, and where to send the answer.
pub(crate) enum Command {
    ListVms(Reply<Vec<VmSummary>>),
    GetVm {
        id: VmId,
        tx: Reply<VmDetail>,
    },
    ListCheckpoints {
        vm: VmId,
        tx: Reply<Vec<Checkpoint>>,
    },
    ListSwitches(Reply<Vec<SwitchSummary>>),
    GetSwitch {
        id: SwitchId,
        tx: Reply<SwitchDetail>,
    },
    ListVhds {
        vm: Option<VmId>,
        tx: Reply<Vec<VhdAttachment>>,
    },
    GetVhd {
        path: String,
        tx: Reply<VhdInfo>,
    },
    RequestVmState {
        id: VmId,
        request: RequestedState,
        tx: Reply<Pending>,
    },
    CreateCheckpoint {
        vm: VmId,
        name: Option<String>,
        tx: Reply<Pending>,
    },
    RestoreCheckpoint {
        vm: VmId,
        checkpoint: CheckpointId,
        tx: Reply<Pending>,
    },
    RemoveCheckpoint {
        checkpoint: CheckpointId,
        tx: Reply<Pending>,
    },
    JobStatus {
        job: JobRef,
        tx: Reply<JobStatus>,
    },
}

impl Command {
    fn run<B: HyperV>(self, backend: &mut B) {
        // A dropped receiver just means the caller gave up; the work is done
        // anyway, so the result is discarded.
        match self {
            Command::ListVms(tx) => {
                let _ = tx.send(backend.list_vms());
            }
            Command::GetVm { id, tx } => {
                let _ = tx.send(backend.get_vm(id));
            }
            Command::ListCheckpoints { vm, tx } => {
                let _ = tx.send(backend.list_checkpoints(vm));
            }
            Command::ListSwitches(tx) => {
                let _ = tx.send(backend.list_switches());
            }
            Command::GetSwitch { id, tx } => {
                let _ = tx.send(backend.get_switch(id));
            }
            Command::ListVhds { vm, tx } => {
                let _ = tx.send(backend.list_vhds(vm));
            }
            Command::GetVhd { path, tx } => {
                let _ = tx.send(backend.get_vhd(&path));
            }
            Command::RequestVmState { id, request, tx } => {
                let _ = tx.send(backend.request_vm_state(id, request));
            }
            Command::CreateCheckpoint { vm, name, tx } => {
                let _ = tx.send(backend.create_checkpoint(vm, name.as_deref()));
            }
            Command::RestoreCheckpoint { vm, checkpoint, tx } => {
                let _ = tx.send(backend.restore_checkpoint(vm, checkpoint));
            }
            Command::RemoveCheckpoint { checkpoint, tx } => {
                let _ = tx.send(backend.remove_checkpoint(checkpoint));
            }
            Command::JobStatus { job, tx } => {
                let _ = tx.send(backend.job_status(job));
            }
        }
    }
}

/// The thread that owns the backend.
pub struct BackendActor;

impl BackendActor {
    /// Builds a backend with `factory` on a new thread and returns the handle
    /// that sends commands to it.
    ///
    /// `factory` runs on the actor thread, so the value it returns never crosses
    /// a thread boundary and does not have to be `Send`.
    pub fn spawn<F, B>(factory: F) -> BackendHandle
    where
        F: FnOnce() -> B + Send + 'static,
        B: HyperV,
    {
        let (commands, mut receiver) = tokio_mpsc::channel::<Command>(ACTOR_CHANNEL_CAPACITY);
        // The actor ends when every handle is dropped (the channel closes).
        thread::spawn(move || {
            let mut backend = factory();
            while let Some(command) = receiver.blocking_recv() {
                command.run(&mut backend);
            }
        });
        BackendHandle::new(commands)
    }
}

/// Sent by a caller when it cannot reach the actor at all.
pub(crate) fn unreachable_error() -> HyperVError {
    HyperVError::backend(RawError::new("the backend actor is not running"))
}

/// Sent by a caller when the actor dies without answering.
pub(crate) fn stopped_error() -> HyperVError {
    HyperVError::backend(RawError::new("the backend actor stopped before replying"))
}
