//! Real-Hyper-V acceptance tests for the WMI backend (M6 reads, M7 mutations).
//!
//! Every test is `#[ignore]`d: they need a Windows host with Hyper-V and a
//! non-empty VM inventory, so they never run on CI. Run them with:
//!
//! ```text
//! cargo test -p hypervctl-backend-wmi --test real_machine -- --ignored
//! ```
//!
//! Design 6.2 applies: mutation tests only touch VMs whose name starts with
//! `hypervctl-test-`. The helper refuses anything else (M7 acceptance).

#![cfg(windows)]

use hypervctl_backend_wmi::WmiBackend;
use hypervctl_core::{HyperV, HyperVError, RequestedState, TargetKind, VmId, VmState};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Prefix every mutation test requires; see design 6.2.
const SAFE_PREFIX: &str = "hypervctl-test-";

/// Serializes mutation tests: they all operate on the same VM, so running
/// them in parallel races on EnabledState and leaves the VM in an unknown
/// state for the next test.
fn mutation_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn backend() -> WmiBackend {
    WmiBackend::connect_or_unavailable()
}

fn must_connect(mut backend: WmiBackend) -> WmiBackend {
    // Prove the connection opened; otherwise every later assertion is noise.
    match backend.list_vms() {
        Ok(_) => backend,
        Err(error) => panic!("cannot reach Hyper-V: {error}"),
    }
}

/// The id of a VM whose name starts with [`SAFE_PREFIX`], or a clear panic.
fn must_find_test_vm(backend: &mut WmiBackend) -> VmId {
    let vms = backend.list_vms().expect("list_vms");
    vms.iter()
        .find(|vm| vm.name.starts_with(SAFE_PREFIX))
        .map(|vm| vm.id)
        .unwrap_or_else(|| {
            panic!("no VM named {SAFE_PREFIX}* exists; create one before running this test")
        })
}

/// Refuses to hand back a VM the mutation tests must not touch.
fn assert_safe_vm(backend: &mut WmiBackend, id: VmId) {
    let detail = backend.get_vm(id).expect("get_vm");
    assert!(
        is_safe_vm_name(&detail.summary.name),
        "refusing to mutate '{}'; only {SAFE_PREFIX}* VMs are safe targets",
        detail.summary.name
    );
}

/// Whether a VM name is inside the mutation sandbox (design 6.2).
fn is_safe_vm_name(name: &str) -> bool {
    name.starts_with(SAFE_PREFIX)
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn list_vms_reads_every_vm_in_one_bulk_query() {
    let mut backend = must_connect(backend());
    let vms = backend.list_vms().expect("list_vms");
    assert!(
        !vms.is_empty(),
        "the host has no VMs; create one named hypervctl-test- first"
    );
    for vm in &vms {
        // Every id is the GUID Hyper-V uses as `Name`.
        assert!(!vm.id.to_string().is_empty());
        assert!(!vm.name.is_empty(), "every VM has a display name");
        // State is one of the mapped values or Unknown(raw); never panics.
        let _ = vm.state.to_string();
        if vm.state == VmState::Off || vm.state == VmState::Saved {
            assert_eq!(vm.uptime, None, "Off/Saved VMs report no uptime");
        }
    }
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn get_vm_returns_the_same_summary_as_the_list() {
    let mut backend = must_connect(backend());
    let listed = backend.list_vms().expect("list_vms");
    let first = listed.first().expect("at least one VM");
    let detail = backend.get_vm(first.id).expect("get_vm");
    assert_eq!(detail.summary.id, first.id);
    assert_eq!(detail.summary.state, first.state);
    // Checkpoint count is a u32 that can be zero.
    let _ = detail.checkpoint_count;
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn an_unknown_vm_id_is_not_found() {
    use hypervctl_core::{HyperVError, VmId};
    let mut backend = must_connect(backend());
    // The nil GUID is never a real VM.
    let error = backend
        .get_vm(VmId::new(uuid::Uuid::nil()))
        .expect_err("must fail");
    assert!(
        matches!(
            error,
            HyperVError::NotFound {
                kind: TargetKind::Vm,
                ..
            } | HyperVError::Backend { .. }
        ),
        "expected NotFound or Backend, got: {error}"
    );
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn list_checkpoints_returns_a_list_for_an_existing_vm() {
    let mut backend = must_connect(backend());
    let listed = backend.list_vms().expect("list_vms");
    let first = listed.first().expect("at least one VM");
    let checkpoints = backend
        .list_checkpoints(first.id)
        .expect("list_checkpoints");
    // Zero checkpoints is a valid answer.
    for checkpoint in &checkpoints {
        assert!(!checkpoint.name.is_empty());
        let _ = checkpoint.id;
    }
    // A nil id is not a VM: either NotFound or an empty list, never a panic.
    let _ = backend.list_checkpoints(VmId::new(uuid::Uuid::nil()));
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn list_switches_reads_the_inventory() {
    let mut backend = must_connect(backend());
    // Zero switches is valid on a fresh host; the call must not error.
    let switches = backend.list_switches().expect("list_switches");
    for switch in &switches {
        assert!(!switch.name.is_empty());
        let _ = switch.kind;
    }
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn list_vhds_reports_attachments_with_controller_slots() {
    let mut backend = must_connect(backend());
    let all = backend.list_vhds(None).expect("list_vhds");
    for attached in &all {
        let att = &attached.attachment;
        // Every attachment must have a real VM id (not nil) and a path.
        assert_ne!(
            att.vm_id,
            VmId::new(uuid::Uuid::nil()),
            "SDRD walk must resolve the owning VM"
        );
        assert!(!att.path.as_os_str().is_empty(), "path must not be empty");
        assert!(
            att.controller.number < 4,
            "controller number {} is implausible",
            att.controller.number
        );
        assert!(
            att.controller.lun < 64,
            "LUN {} is implausible",
            att.controller.lun
        );
    }
    // Filtering by a known VM returns only that VM's disks.
    let listed = backend.list_vms().expect("list_vms");
    if let Some(first) = listed.first() {
        let filtered = backend.list_vhds(Some(first.id)).expect("filter");
        for attached in &filtered {
            assert_eq!(attached.attachment.vm_id, first.id);
        }
    }
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn vhd_info_reports_virtual_size_larger_than_or_equal_to_file_size() {
    let mut backend = must_connect(backend());
    let all = backend.list_vhds(None).expect("list_vhds");
    let with_info: Vec<_> = all.iter().filter(|a| a.info.is_some()).collect();
    if with_info.is_empty() {
        // No readable VHD files on this host; nothing to assert.
        return;
    }
    for attached in &with_info {
        let info = attached.info.as_ref().expect("info");
        // Virtual size is never smaller than the file size for a valid VHD/VHDX.
        assert!(
            info.virtual_size_bytes >= info.file_size_bytes,
            "virtual size {} < file size {} for {:?}",
            info.virtual_size_bytes,
            info.file_size_bytes,
            info.path
        );
        // A real disk is at least 1 MB.
        assert!(
            info.virtual_size_bytes >= 1024 * 1024,
            "virtual size {} is implausibly small for {:?}",
            info.virtual_size_bytes,
            info.path
        );
    }
}

/// Waits until `id` reaches `goal`, or fails with the last observed state.
fn wait_for_state(backend: &mut WmiBackend, id: VmId, goal: VmState) {
    for _ in 0..60 {
        let state = backend.get_vm(id).expect("get_vm").summary.state;
        if state == goal {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let last = backend.get_vm(id).expect("get_vm").summary.state;
    panic!("timed out waiting for {goal}; last state was {last}");
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn the_safety_helper_refuses_a_vm_outside_the_test_prefix() {
    let mut backend = must_connect(backend());
    // Pure check: a foreign name is refused, a test-prefixed name is accepted.
    assert!(!is_safe_vm_name("Windows11"));
    assert!(!is_safe_vm_name("WS2025"));
    assert!(is_safe_vm_name("hypervctl-test-debian"));
    assert!(is_safe_vm_name("hypervctl-test-anything"));

    // On this host, prove the guard against a real VM when one exists.
    let listed = backend.list_vms().expect("list_vms");
    if let Some(foreign) = listed
        .iter()
        .find(|vm| !is_safe_vm_name(&vm.name))
        .map(|vm| vm.id)
    {
        // The helper panics for this id; we only prove the lookup finds it.
        let detail = backend.get_vm(foreign).expect("get_vm");
        assert!(!is_safe_vm_name(&detail.summary.name));
    }
    let safe = must_find_test_vm(&mut backend);
    assert_safe_vm(&mut backend, safe);
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn power_operations_start_and_turn_off_a_test_vm() {
    let _guard = mutation_lock();
    let mut backend = must_connect(backend());
    let id = must_find_test_vm(&mut backend);
    assert_safe_vm(&mut backend, id);

    // Bring the VM to a known starting point regardless of prior state.
    let initial = backend.get_vm(id).expect("get_vm").summary.state;
    if initial != VmState::Off {
        let off = backend
            .request_vm_state(id, RequestedState::TurnOff)
            .expect("turn off");
        assert!(
            matches!(
                off,
                hypervctl_core::Pending::Job(_) | hypervctl_core::Pending::Done
            ),
            "turn off should be Job or Done, got {off:?}"
        );
        wait_for_state(&mut backend, id, VmState::Off);
    }

    // Off → Start → Running → TurnOff → Off. Each step must accept the
    // request (Pending::Job or Done); completion is EnabledState polling.
    let start = backend
        .request_vm_state(id, RequestedState::Start)
        .expect("start");
    assert!(
        matches!(
            start,
            hypervctl_core::Pending::Job(_) | hypervctl_core::Pending::Done
        ),
        "start should be Job or Done, got {start:?}"
    );
    wait_for_state(&mut backend, id, VmState::Running);

    let off = backend
        .request_vm_state(id, RequestedState::TurnOff)
        .expect("turn off");
    assert!(
        matches!(
            off,
            hypervctl_core::Pending::Job(_) | hypervctl_core::Pending::Done
        ),
        "turn off should be Job or Done, got {off:?}"
    );
    wait_for_state(&mut backend, id, VmState::Off);
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn checkpoint_create_list_and_remove_round_trip() {
    let _guard = mutation_lock();
    let mut backend = must_connect(backend());
    let id = must_find_test_vm(&mut backend);
    assert_safe_vm(&mut backend, id);

    // Ensure Off so CreateSnapshot is legal and the test is deterministic.
    let initial = backend.get_vm(id).expect("get_vm").summary.state;
    if initial != VmState::Off {
        let off = backend
            .request_vm_state(id, RequestedState::TurnOff)
            .expect("turn off");
        assert!(
            matches!(
                off,
                hypervctl_core::Pending::Job(_) | hypervctl_core::Pending::Done
            ),
            "turn off should be Job or Done, got {off:?}"
        );
        wait_for_state(&mut backend, id, VmState::Off);
    }

    let before = backend.list_checkpoints(id).expect("list_checkpoints");
    let created = backend.create_checkpoint(id).expect("create_checkpoint");
    let job = match created {
        hypervctl_core::Pending::Job(job) => job,
        other => panic!("CreateSnapshot should start a job, got {other:?}"),
    };

    // Poll the job until it finishes (M0 item 6: JobState 7=Completed).
    for _ in 0..120 {
        let status = backend.job_status(job.clone()).expect("job_status");
        if status.is_finished() {
            match status {
                hypervctl_core::JobStatus::Succeeded => break,
                hypervctl_core::JobStatus::Failed {
                    code,
                    message,
                    reason,
                    ..
                } => {
                    panic!("checkpoint job failed: {code} {message} ({reason})");
                }
                other => panic!("checkpoint job ended as {other:?}"),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    let after = backend.list_checkpoints(id).expect("list_checkpoints");
    assert_eq!(
        after.len(),
        before.len() + 1,
        "one checkpoint should have been created"
    );
    let fresh = after.last().expect("the new checkpoint");

    // Remove the checkpoint we just created.
    let removed = backend
        .remove_checkpoint(fresh.id)
        .expect("remove_checkpoint");
    let job = match removed {
        hypervctl_core::Pending::Job(job) => job,
        other => panic!("DestroySnapshot should start a job, got {other:?}"),
    };
    for _ in 0..120 {
        let status = backend.job_status(job.clone()).expect("job_status");
        if status.is_finished() {
            match status {
                hypervctl_core::JobStatus::Succeeded => break,
                hypervctl_core::JobStatus::Failed {
                    code,
                    message,
                    reason,
                    ..
                } => {
                    panic!("destroy job failed: {code} {message} ({reason})");
                }
                other => panic!("destroy job ended as {other:?}"),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    let final_list = backend.list_checkpoints(id).expect("list_checkpoints");
    assert_eq!(
        final_list.len(),
        before.len(),
        "the checkpoint should have been removed"
    );
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn job_status_reports_unknown_jobs_as_backend_errors() {
    let mut backend = must_connect(backend());
    use hypervctl_core::JobRef;
    let error = backend
        .job_status(JobRef::from("Microsoft:not-a-real-job"))
        .expect_err("must fail");
    // Either NotFound or a backend error; never a panic.
    assert!(
        matches!(
            error,
            HyperVError::NotFound { .. } | HyperVError::Backend { .. }
        ),
        "expected NotFound or Backend, got: {error}"
    );
}
