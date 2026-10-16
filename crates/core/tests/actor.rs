//! Actor/handle behaviour that the design calls out by name (design 3.2).

use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use hypervctl_core::{
    AttachedVhd, BackendActor, ChangeEvent, Checkpoint, CheckpointId, HyperV, HyperVError, JobRef,
    JobStatus, Pending, RawError, RequestedState, SwitchDetail, SwitchId, SwitchSummary, Thumbnail,
    VhdInfo, VmDetail, VmId, VmState, VmSummary,
};

fn no_notifications() -> Result<tokio::sync::mpsc::UnboundedReceiver<ChangeEvent>, HyperVError> {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    Ok(rx)
}

fn unused() -> HyperVError {
    HyperVError::backend(RawError::new("not exercised by this test"))
}

/// A backend that is deliberately `!Send`: it may only ever be built and used
/// on the actor thread.
struct NotSendBackend {
    _not_send: Rc<()>,
    factory_thread: mpsc::Sender<thread::ThreadId>,
    vms: Vec<VmSummary>,
}

impl HyperV for NotSendBackend {
    fn list_vms(&mut self) -> Result<Vec<VmSummary>, HyperVError> {
        let _ = self.factory_thread.send(thread::current().id());
        Ok(self.vms.clone())
    }

    fn get_thumbnail(
        &mut self,
        _id: VmId,
        _width: u16,
        _height: u16,
    ) -> Result<Thumbnail, HyperVError> {
        Err(unused())
    }

    fn get_vm(&mut self, _id: VmId) -> Result<VmDetail, HyperVError> {
        Err(unused())
    }

    fn list_checkpoints(&mut self, _vm: VmId) -> Result<Vec<Checkpoint>, HyperVError> {
        Ok(Vec::new())
    }

    fn list_switches(&mut self) -> Result<Vec<SwitchSummary>, HyperVError> {
        Ok(Vec::new())
    }

    fn get_switch(&mut self, _id: SwitchId) -> Result<SwitchDetail, HyperVError> {
        Err(unused())
    }

    fn list_vhds(&mut self, _vm: Option<VmId>) -> Result<Vec<AttachedVhd>, HyperVError> {
        Ok(Vec::new())
    }

    fn get_vhd(&mut self, _path: &str) -> Result<VhdInfo, HyperVError> {
        Err(unused())
    }

    fn request_vm_state(
        &mut self,
        _id: VmId,
        _request: RequestedState,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn request_guest_reboot(&mut self, _id: VmId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn create_checkpoint(&mut self, _vm: VmId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn rename_checkpoint(
        &mut self,
        _checkpoint: CheckpointId,
        _name: &str,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn restore_checkpoint(
        &mut self,
        _vm: VmId,
        _checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn remove_checkpoint(&mut self, _checkpoint: CheckpointId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn job_status(&mut self, _job: JobRef) -> Result<JobStatus, HyperVError> {
        Err(unused())
    }

    fn subscribe_changes(
        &mut self,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<ChangeEvent>, HyperVError> {
        no_notifications()
    }
}

/// A backend that dies the first time it is asked for the VM list.
struct PanickingBackend {
    calls: u32,
}

impl HyperV for PanickingBackend {
    fn list_vms(&mut self) -> Result<Vec<VmSummary>, HyperVError> {
        self.calls += 1;
        if self.calls == 1 {
            panic!("simulated backend panic");
        }
        Ok(Vec::new())
    }

    fn get_thumbnail(
        &mut self,
        _id: VmId,
        _width: u16,
        _height: u16,
    ) -> Result<Thumbnail, HyperVError> {
        Err(unused())
    }

    fn get_vm(&mut self, _id: VmId) -> Result<VmDetail, HyperVError> {
        Err(unused())
    }

    fn list_checkpoints(&mut self, _vm: VmId) -> Result<Vec<Checkpoint>, HyperVError> {
        Ok(Vec::new())
    }

    fn list_switches(&mut self) -> Result<Vec<SwitchSummary>, HyperVError> {
        Ok(Vec::new())
    }

    fn get_switch(&mut self, _id: SwitchId) -> Result<SwitchDetail, HyperVError> {
        Err(unused())
    }

    fn list_vhds(&mut self, _vm: Option<VmId>) -> Result<Vec<AttachedVhd>, HyperVError> {
        Ok(Vec::new())
    }

    fn get_vhd(&mut self, _path: &str) -> Result<VhdInfo, HyperVError> {
        Err(unused())
    }

    fn request_vm_state(
        &mut self,
        _id: VmId,
        _request: RequestedState,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn request_guest_reboot(&mut self, _id: VmId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn create_checkpoint(&mut self, _vm: VmId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn rename_checkpoint(
        &mut self,
        _checkpoint: CheckpointId,
        _name: &str,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn restore_checkpoint(
        &mut self,
        _vm: VmId,
        _checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn remove_checkpoint(&mut self, _checkpoint: CheckpointId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn job_status(&mut self, _job: JobRef) -> Result<JobStatus, HyperVError> {
        Err(unused())
    }

    fn subscribe_changes(
        &mut self,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<ChangeEvent>, HyperVError> {
        no_notifications()
    }
}

fn sample_vm(id: u128, name: &str) -> VmSummary {
    VmSummary {
        id: VmId::new(uuid::Uuid::from_u128(id)),
        name: name.to_string(),
        state: VmState::Running,
        cpu_usage_percent: Some(1.0),
        memory_assigned_bytes: Some(1024 * 1024 * 1024),
        uptime: Some(Duration::from_secs(60)),
        generation: Some(2),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_factory_runs_on_the_actor_thread_and_may_return_a_non_send_value() {
    let (reported, factory_result) = mpsc::channel();
    let vms = vec![sample_vm(1, "web-01"), sample_vm(2, "web-02")];
    let handle = BackendActor::spawn(move || NotSendBackend {
        _not_send: Rc::new(()),
        factory_thread: reported,
        vms,
    });

    let listed = handle.list_vms().await;
    assert_eq!(
        listed,
        Ok(vec![sample_vm(1, "web-01"), sample_vm(2, "web-02")])
    );

    let factory_thread = factory_result.recv_timeout(Duration::from_secs(2));
    assert!(
        factory_thread.is_ok(),
        "the factory must run on the actor thread"
    );
    assert_ne!(
        factory_thread.unwrap_or_else(|_| thread::current().id()),
        thread::current().id(),
        "the factory must not run on the caller's thread"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_actor_answers_instead_of_leaving_the_caller_waiting() {
    // Acceptance test 14 of the plan: after the actor dies, calls must return a
    // `Backend` error quickly instead of blocking.
    let handle = BackendActor::spawn(|| PanickingBackend { calls: 0 });

    let first = tokio::time::timeout(Duration::from_secs(2), handle.list_vms()).await;
    assert!(
        matches!(first, Ok(Err(HyperVError::Backend { .. }))),
        "the panicking call must surface as a backend error, got {first:?}"
    );

    let second = tokio::time::timeout(Duration::from_secs(2), handle.list_vms()).await;
    assert!(
        matches!(second, Ok(Err(HyperVError::Backend { .. }))),
        "later calls must fail fast, got {second:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cloned_handles_share_one_actor() {
    let (reported, factory_result) = mpsc::channel();
    let handle = BackendActor::spawn(move || NotSendBackend {
        _not_send: Rc::new(()),
        factory_thread: reported,
        vms: vec![sample_vm(1, "web-01")],
    });
    let second = handle.clone();

    assert_eq!(handle.list_vms().await, Ok(vec![sample_vm(1, "web-01")]));
    assert_eq!(second.list_vms().await, Ok(vec![sample_vm(1, "web-01")]));
    assert!(factory_result.recv_timeout(Duration::from_secs(2)).is_ok());
}
