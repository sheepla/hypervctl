//! The backend contract: what the frontends may ask Hyper-V (or a stand-in) to do.
//!
//! Methods are synchronous and never wait: a mutation either finishes at once,
//! starts a job, or is finished when the VM reaches a target state (design 3.1).

use std::fmt;

use tokio::sync::mpsc;

use crate::checkpoint::{Checkpoint, CheckpointId};
use crate::error::HyperVError;
use crate::events::ChangeEvent;
use crate::ids::{SwitchId, VmId};
use crate::switch::{SwitchDetail, SwitchSummary};
use crate::vhd::{AttachedVhd, VhdInfo};
use crate::vm::{VmDetail, VmSummary};

use crate::job::{JobRef, JobStatus, Pending};

/// A state a caller may ask a VM to enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestedState {
    Start,
    ShutDown,
    TurnOff,
    Reset,
    Save,
    Pause,
    Resume,
}

impl RequestedState {
    pub const ALL: [RequestedState; 7] = [
        RequestedState::Start,
        RequestedState::ShutDown,
        RequestedState::TurnOff,
        RequestedState::Reset,
        RequestedState::Save,
        RequestedState::Pause,
        RequestedState::Resume,
    ];
}

impl fmt::Display for RequestedState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::ShutDown => f.write_str("shutdown"),
            Self::TurnOff => f.write_str("turn off"),
            Self::Reset => f.write_str("reset"),
            Self::Save => f.write_str("save"),
            Self::Pause => f.write_str("pause"),
            Self::Resume => f.write_str("resume"),
        }
    }
}

/// The synchronous backend contract. The frontends never call it directly; they
/// go through [`crate::BackendHandle`](crate::BackendHandle).
///
/// Reads may be called from any command. Mutations must accept the "already
/// there" case as a normal result: de-duplication is the operation layer's job
/// (design 3.3), so the backend just reports what Hyper-V answered.
pub trait HyperV {
    fn list_vms(&mut self) -> Result<Vec<VmSummary>, HyperVError>;

    fn get_vm(&mut self, id: VmId) -> Result<VmDetail, HyperVError>;

    fn list_checkpoints(&mut self, vm: VmId) -> Result<Vec<Checkpoint>, HyperVError>;

    fn list_switches(&mut self) -> Result<Vec<SwitchSummary>, HyperVError>;

    fn get_switch(&mut self, id: SwitchId) -> Result<SwitchDetail, HyperVError>;

    /// Every attachment with its file information, or only those of one VM
    /// when `vm` is given (design 3.1).
    fn list_vhds(&mut self, vm: Option<VmId>) -> Result<Vec<AttachedVhd>, HyperVError>;

    /// Disk information for a path, whether or not it is attached.
    fn get_vhd(&mut self, path: &str) -> Result<VhdInfo, HyperVError>;

    fn request_vm_state(
        &mut self,
        id: VmId,
        request: RequestedState,
    ) -> Result<Pending, HyperVError>;

    fn create_checkpoint(&mut self, vm: VmId, name: Option<&str>) -> Result<Pending, HyperVError>;

    fn restore_checkpoint(
        &mut self,
        vm: VmId,
        checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError>;

    fn remove_checkpoint(&mut self, checkpoint: CheckpointId) -> Result<Pending, HyperVError>;

    fn job_status(&mut self, job: JobRef) -> Result<JobStatus, HyperVError>;

    /// Starts a stream of change notifications (design 3.4). Every subscription
    /// is independent: re-subscribing is how the frontend recovers from a lost
    /// stream, so the backend must not assume it is called only once.
    fn subscribe_changes(&mut self) -> Result<mpsc::UnboundedReceiver<ChangeEvent>, HyperVError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_states_have_stable_names() {
        assert_eq!(RequestedState::ShutDown.to_string(), "shutdown");
        assert_eq!(RequestedState::ALL.len(), 7);
        let mut seen = RequestedState::ALL.to_vec();
        seen.sort_by_key(|state| state.to_string());
        seen.dedup();
        assert_eq!(seen.len(), RequestedState::ALL.len());
    }

    #[test]
    fn the_trait_is_object_safe() {
        fn assert_object_safe(_: &dyn HyperV) {}
        let stub = MockTraitStub;
        assert_object_safe(&stub);
    }

    struct MockTraitStub;

    impl HyperV for MockTraitStub {
        fn list_vms(&mut self) -> Result<Vec<VmSummary>, HyperVError> {
            Ok(Vec::new())
        }

        fn get_vm(&mut self, _id: VmId) -> Result<VmDetail, HyperVError> {
            Err(HyperVError::backend(crate::error::RawError::new("stub")))
        }

        fn list_checkpoints(&mut self, _vm: VmId) -> Result<Vec<Checkpoint>, HyperVError> {
            Ok(Vec::new())
        }

        fn list_switches(&mut self) -> Result<Vec<SwitchSummary>, HyperVError> {
            Ok(Vec::new())
        }

        fn get_switch(&mut self, _id: SwitchId) -> Result<SwitchDetail, HyperVError> {
            Err(HyperVError::backend(crate::error::RawError::new("stub")))
        }

        fn list_vhds(&mut self, _vm: Option<VmId>) -> Result<Vec<AttachedVhd>, HyperVError> {
            Ok(Vec::new())
        }

        fn get_vhd(&mut self, _path: &str) -> Result<VhdInfo, HyperVError> {
            Err(HyperVError::backend(crate::error::RawError::new("stub")))
        }

        fn request_vm_state(
            &mut self,
            _id: VmId,
            _request: RequestedState,
        ) -> Result<Pending, HyperVError> {
            Err(HyperVError::backend(crate::error::RawError::new("stub")))
        }

        fn create_checkpoint(
            &mut self,
            _vm: VmId,
            _name: Option<&str>,
        ) -> Result<Pending, HyperVError> {
            Err(HyperVError::backend(crate::error::RawError::new("stub")))
        }

        fn restore_checkpoint(
            &mut self,
            _vm: VmId,
            _checkpoint: CheckpointId,
        ) -> Result<Pending, HyperVError> {
            Err(HyperVError::backend(crate::error::RawError::new("stub")))
        }

        fn remove_checkpoint(&mut self, _checkpoint: CheckpointId) -> Result<Pending, HyperVError> {
            Err(HyperVError::backend(crate::error::RawError::new("stub")))
        }

        fn job_status(&mut self, _job: JobRef) -> Result<JobStatus, HyperVError> {
            Err(HyperVError::backend(crate::error::RawError::new("stub")))
        }

        fn subscribe_changes(
            &mut self,
        ) -> Result<mpsc::UnboundedReceiver<ChangeEvent>, HyperVError> {
            let (_tx, rx) = mpsc::unbounded_channel();
            Ok(rx)
        }
    }
}
