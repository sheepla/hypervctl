//! The handle the frontends hold. Cheap to clone; every call goes to the actor.

use tokio::sync::{mpsc, oneshot};

use crate::actor::{self, Command};
use crate::checkpoint::{Checkpoint, CheckpointId};
use crate::error::HyperVError;
use crate::events::ChangeEvent;
use crate::hyper_v::RequestedState;
use crate::ids::{SwitchId, VmId};
use crate::job::{JobRef, JobStatus, Pending};
use crate::switch::{SwitchDetail, SwitchSummary};
use crate::vhd::{AttachedVhd, VhdInfo};
use crate::vm::{VmDetail, VmSummary};

/// Async front door to a backend running on its own thread (design 3.2).
#[derive(Debug, Clone)]
pub struct BackendHandle {
    commands: mpsc::Sender<Command>,
}

impl BackendHandle {
    pub(crate) fn new(commands: mpsc::Sender<Command>) -> Self {
        Self { commands }
    }

    /// Sends one command and waits for its answer.
    ///
    /// A dead actor fails fast: the queue rejects the command, or the reply
    /// channel closes. Neither path waits forever (design 3.2, acceptance 14).
    async fn call<T>(
        &self,
        make: impl FnOnce(actor::Reply<T>) -> Command,
    ) -> Result<T, HyperVError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(make(tx))
            .await
            .map_err(|_| actor::unreachable_error())?;
        // The reply carries the backend's own answer; a closed channel means
        // the actor stopped without answering.
        match rx.await {
            Ok(answer) => answer,
            Err(_) => Err(actor::stopped_error()),
        }
    }

    pub async fn list_vms(&self) -> Result<Vec<VmSummary>, HyperVError> {
        self.call(Command::ListVms).await
    }

    pub async fn get_vm(&self, id: VmId) -> Result<VmDetail, HyperVError> {
        self.call(|tx| Command::GetVm { id, tx }).await
    }

    pub async fn list_checkpoints(&self, vm: VmId) -> Result<Vec<Checkpoint>, HyperVError> {
        self.call(|tx| Command::ListCheckpoints { vm, tx }).await
    }

    pub async fn list_switches(&self) -> Result<Vec<SwitchSummary>, HyperVError> {
        self.call(Command::ListSwitches).await
    }

    pub async fn get_switch(&self, id: SwitchId) -> Result<SwitchDetail, HyperVError> {
        self.call(|tx| Command::GetSwitch { id, tx }).await
    }

    pub async fn list_vhds(&self, vm: Option<VmId>) -> Result<Vec<AttachedVhd>, HyperVError> {
        self.call(|tx| Command::ListVhds { vm, tx }).await
    }

    pub async fn get_vhd(&self, path: &str) -> Result<VhdInfo, HyperVError> {
        let path = path.to_string();
        self.call(|tx| Command::GetVhd { path, tx }).await
    }

    pub async fn request_vm_state(
        &self,
        id: VmId,
        request: RequestedState,
    ) -> Result<Pending, HyperVError> {
        self.call(|tx| Command::RequestVmState { id, request, tx })
            .await
    }

    pub async fn request_guest_reboot(&self, id: VmId) -> Result<Pending, HyperVError> {
        self.call(|tx| Command::RequestGuestReboot { id, tx }).await
    }

    pub async fn create_checkpoint(&self, vm: VmId) -> Result<Pending, HyperVError> {
        self.call(|tx| Command::CreateCheckpoint { vm, tx }).await
    }

    pub async fn rename_checkpoint(
        &self,
        checkpoint: CheckpointId,
        name: &str,
    ) -> Result<Pending, HyperVError> {
        let name = name.to_string();
        self.call(|tx| Command::RenameCheckpoint {
            checkpoint,
            name,
            tx,
        })
        .await
    }

    pub async fn restore_checkpoint(
        &self,
        vm: VmId,
        checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError> {
        self.call(|tx| Command::RestoreCheckpoint { vm, checkpoint, tx })
            .await
    }

    pub async fn remove_checkpoint(
        &self,
        checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError> {
        self.call(|tx| Command::RemoveCheckpoint { checkpoint, tx })
            .await
    }

    pub async fn job_status(&self, job: JobRef) -> Result<JobStatus, HyperVError> {
        self.call(|tx| Command::JobStatus { job, tx }).await
    }

    /// Starts a stream of change notifications (design 3.4).
    pub async fn subscribe_changes(
        &self,
    ) -> Result<mpsc::UnboundedReceiver<ChangeEvent>, HyperVError> {
        self.call(Command::SubscribeChanges).await
    }
}
