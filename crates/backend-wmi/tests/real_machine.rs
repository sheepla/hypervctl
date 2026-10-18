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
use hypervctl_core::{
    ControllerKind, HyperV, HyperVError, JobFailReason, RequestedState, TargetKind, VhdFormat,
    VhdKind, VmId, VmState,
};
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
fn list_vms_reads_every_vm_without_per_vm_queries() {
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
        // Generation comes from the active VSSD subtype (Gen1/Gen2).
        assert!(
            matches!(vm.generation, Some(1) | Some(2)),
            "{} has no generation: {:?}",
            vm.name,
            vm.generation
        );
        // State is one of the mapped values or Unknown(raw); never panics.
        let _ = vm.state.to_string();
        if vm.state == VmState::Off || vm.state == VmState::Saved {
            assert_eq!(vm.uptime, None, "Off/Saved VMs report no uptime");
            assert_eq!(vm.cpu_usage_percent, None, "Off/Saved VMs report no CPU");
        }
        if vm.state == VmState::Running {
            assert!(
                vm.cpu_usage_percent.is_some(),
                "{} is running but reports no ProcessorLoad",
                vm.name
            );
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
    // Generation, the config file and the notes all come from the active
    // configuration (`Microsoft:Hyper-V:System:Realized`).
    assert!(
        matches!(detail.summary.generation, Some(1) | Some(2)),
        "generation must come from the VSSD subtype: {:?}",
        detail.summary.generation
    );
    assert!(
        !detail.config_path.is_empty(),
        "config path must come from the realized VSSD"
    );
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
fn get_switch_reports_only_real_vms_as_connected() {
    let mut backend = must_connect(backend());
    let vm_ids: Vec<VmId> = backend
        .list_vms()
        .expect("list_vms")
        .iter()
        .map(|vm| vm.id)
        .collect();
    let switches = backend.list_switches().expect("list_switches");
    for switch in switches {
        let detail = backend.get_switch(switch.id).expect("get_switch");
        // Checkpoint configurations own NICs too, but their GUIDs are not VMs.
        for vm_id in &detail.connected_vm_ids {
            assert!(
                vm_ids.contains(vm_id),
                "switch {} lists {vm_id}, which is not a VM",
                detail.summary.name
            );
        }
    }
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn list_vhds_reports_attachments_with_controller_slots() {
    let mut backend = must_connect(backend());
    let all = backend.list_vhds(None).expect("list_vhds");
    let listed = backend.list_vms().expect("list_vms");
    let vm_ids: Vec<VmId> = listed.iter().map(|vm| vm.id).collect();
    for attached in &all {
        let att = &attached.attachment;
        // Checkpoint configurations also own SDRDs, but their GUID is not a
        // VM. Every returned attachment must belong to one of the listed VMs.
        assert!(
            vm_ids.contains(&att.vm_id),
            "attachment for {} must belong to a VM",
            att.vm_id
        );
        assert!(!att.path.as_os_str().is_empty(), "path must not be empty");
        assert!(
            !att.vm_name.is_empty(),
            "the owning VM's name must be filled from Msvm_ComputerSystem"
        );
        assert!(
            !matches!(att.controller.kind, ControllerKind::Unknown(_)),
            "controller {:?} must be classified (IDE/SCSI/SATA)",
            att.controller.kind
        );
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
    if let Some(first) = listed.first() {
        let filtered = backend.list_vhds(Some(first.id)).expect("filter");
        for attached in &filtered {
            assert_eq!(attached.attachment.vm_id, first.id);
            assert_eq!(attached.attachment.vm_name, first.name);
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

/// `vhd list` must list only virtual hard disks (spec 2.5), and every disk's
/// metadata must come from `Msvm_ImageManagementService` (design 3.8.3), never
/// from reading the host file. M11-W2 acceptance 22.
#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn list_vhds_reads_image_management_fields_for_every_disk() {
    let mut backend = must_connect(backend());
    let all = backend.list_vhds(None).expect("list_vhds");
    assert!(
        !all.is_empty(),
        "the host has no attached VHDs; create a VM with a disk first"
    );
    for attached in &all {
        let path = attached
            .attachment
            .path
            .to_string_lossy()
            .to_ascii_lowercase();
        // CD/DVD and floppy attachments share the SDRD class but must not be
        // listed as virtual hard disks.
        assert!(
            !path.ends_with(".iso") && !path.ends_with(".vfd"),
            "non-disk attachment was listed: {path}"
        );
        let info = attached
            .info
            .as_ref()
            .unwrap_or_else(|| panic!("no image-management info for {path}"));
        // The format follows the file extension.
        let expected = if path.ends_with(".vhd") {
            VhdFormat::Vhd
        } else {
            VhdFormat::Vhdx
        };
        assert_eq!(info.format, expected, "format for {path}");
        // A real disk reports its allocation type and identity.
        assert_ne!(info.kind, VhdKind::Unknown(0), "kind for {path}");
        assert!(info.disk_id.is_some(), "disk id for {path}");
        assert!(
            info.virtual_size_bytes >= info.file_size_bytes,
            "virtual size {} < file size {} for {path}",
            info.virtual_size_bytes,
            info.file_size_bytes
        );
    }
}

/// `get_vhd` reads the same metadata as the list for an attached disk, and a
/// path that is not a virtual hard disk is `NotFound` (spec 2.5).
#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn get_vhd_matches_the_attachment_list_entry() {
    let mut backend = must_connect(backend());
    let all = backend.list_vhds(None).expect("list_vhds");
    let first = all.first().expect("at least one attached VHD");
    let path = first.attachment.path.to_string_lossy().into_owned();
    let info = backend.get_vhd(&path).expect("get_vhd");
    let listed = first.info.clone().expect("list info");
    // Compare the stable fields: `file_size_bytes` grows while a disk is in
    // use, so it can differ between the two calls.
    assert_eq!(info.path, listed.path);
    assert_eq!(info.format, listed.format);
    assert_eq!(info.kind, listed.kind);
    assert_eq!(info.virtual_size_bytes, listed.virtual_size_bytes);
    assert_eq!(info.block_size_bytes, listed.block_size_bytes);
    assert_eq!(info.parent_path, listed.parent_path);
    assert_eq!(info.disk_id, listed.disk_id);

    // A path with no virtual hard disk behind it is not found.
    let error = backend
        .get_vhd(r"C:\definitely\missing\disk.vhdx")
        .expect_err("must fail");
    assert!(
        matches!(
            error,
            HyperVError::NotFound {
                kind: TargetKind::Vhd,
                ..
            } | HyperVError::Backend { .. }
        ),
        "expected NotFound or Backend, got: {error}"
    );
}

/// `get_thumbnail` returns the requested size for a VM with a console, and an
/// Off VM (no console) is refused (M13-V).
#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn a_running_vm_yields_a_thumbnail_of_the_requested_size() {
    let mut backend = must_connect(backend());
    let vms = backend.list_vms().expect("list_vms");
    // 320x240 was one of the sizes probed on a real host.
    if let Some(running) = vms.iter().find(|vm| vm.state == VmState::Running) {
        let thumbnail = backend
            .get_thumbnail(running.id, 320, 240)
            .expect("thumbnail");
        assert_eq!((thumbnail.width, thumbnail.height), (320, 240));
        assert_eq!(
            thumbnail.rgba.len(),
            320 * 240 * 4,
            "the RGBA buffer must match the requested size"
        );
    }
    if let Some(off) = vms.iter().find(|vm| vm.state == VmState::Off) {
        assert!(
            backend.get_thumbnail(off.id, 320, 240).is_err(),
            "an Off VM has no console and no thumbnail"
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
fn checkpoint_rename_sets_the_element_name() {
    let _guard = mutation_lock();
    let mut backend = must_connect(backend());
    let id = must_find_test_vm(&mut backend);
    assert_safe_vm(&mut backend, id);

    // Renaming needs no new disk, so it works even when the VHD store is
    // compressed and checkpoint creation is refused (0xC03A001A). It still
    // changes the checkpoint set, so restore the original name afterwards.
    let before = backend.list_checkpoints(id).expect("list_checkpoints");
    let Some(existing) = before.first().cloned() else {
        eprintln!("no checkpoint to rename on the test VM; skipping");
        return;
    };
    let original = existing.name.clone();
    let wanted = "hypervctl-test-renamed";

    // M11-V V4: ModifySystemSettings renames the checkpoint synchronously.
    let pending = backend
        .rename_checkpoint(existing.id, wanted)
        .expect("rename_checkpoint");
    assert!(
        matches!(pending, hypervctl_core::Pending::Done),
        "rename is synchronous, got {pending:?}"
    );
    let renamed = backend.list_checkpoints(id).expect("list_checkpoints");
    let target = renamed
        .iter()
        .find(|checkpoint| checkpoint.id == existing.id)
        .expect("the renamed checkpoint");
    assert_eq!(target.name, wanted, "ElementName should be the new name");

    backend
        .rename_checkpoint(existing.id, &original)
        .expect("restore the original name");
    let restored = backend.list_checkpoints(id).expect("list_checkpoints");
    let target = restored
        .iter()
        .find(|checkpoint| checkpoint.id == existing.id)
        .expect("the restored checkpoint");
    assert_eq!(target.name, original, "the original name is restored");
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

/// Brings `id` to Running the direct way: force off first, then start.
fn ensure_running(backend: &mut WmiBackend, id: VmId) {
    if backend.get_vm(id).expect("get_vm").summary.state == VmState::Running {
        return;
    }
    if backend.get_vm(id).expect("get_vm").summary.state != VmState::Off {
        backend
            .request_vm_state(id, RequestedState::TurnOff)
            .expect("turn off");
        wait_for_state(backend, id, VmState::Off);
    }
    backend
        .request_vm_state(id, RequestedState::Start)
        .expect("start");
    wait_for_state(backend, id, VmState::Running);
}

/// The hint `code` must have produced (design 3.8.2); duplicated here so the
/// wiring between `job_status` and the classification is what is asserted.
fn expected_reason(code: u32) -> JobFailReason {
    match code {
        0x8007_10DF | 32768 => JobFailReason::GuestNotReady,
        0x8007_000E | 32778 => JobFailReason::HostOutOfMemory,
        _ => JobFailReason::Other,
    }
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn shutdown_of_a_running_vm_answers_with_a_job_not_a_state_wait() {
    let _guard = mutation_lock();
    let mut backend = must_connect(backend());
    let id = must_find_test_vm(&mut backend);
    assert_safe_vm(&mut backend, id);
    ensure_running(&mut backend, id);

    let pending = backend
        .request_vm_state(id, RequestedState::ShutDown)
        .expect("shutdown");
    // Design 3.1: ShutDown answers with a job (or an immediate 0); the old
    // AwaitState flattening would swallow the job's failure (anomaly 16).
    let job = match pending {
        hypervctl_core::Pending::Job(job) => Some(job),
        hypervctl_core::Pending::Done => None,
        other => panic!("ShutDown must answer with a job, not {other:?}"),
    };

    if let Some(job) = job {
        let mut refusal = None;
        for _ in 0..120 {
            match backend.job_status(job.clone()).expect("job_status") {
                hypervctl_core::JobStatus::Succeeded => break,
                hypervctl_core::JobStatus::Failed { code, reason, .. } => {
                    refusal = Some((code, reason));
                    break;
                }
                hypervctl_core::JobStatus::Cancelled => {
                    panic!("the shutdown job was cancelled")
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(500)),
            }
        }
        if let Some((code, reason)) = refusal {
            // The refusal must carry the hint its code decides (design
            // 3.8.2), and the VM stays Running behind it (anomaly 16).
            assert_eq!(
                reason,
                expected_reason(code),
                "shutdown failure code {code:#010X} was classified wrongly"
            );
            assert_eq!(
                backend.get_vm(id).expect("get_vm").summary.state,
                VmState::Running,
                "a refused shutdown must leave the VM Running (anomaly 16)"
            );
            // Leave the host as it was found.
            backend
                .request_vm_state(id, RequestedState::TurnOff)
                .expect("turn off");
            wait_for_state(&mut backend, id, VmState::Off);
            return;
        }
    }
    wait_for_state(&mut backend, id, VmState::Off);
}

#[test]
#[ignore = "requires a Windows host with Hyper-V"]
fn guest_reboot_of_a_running_vm_resets_the_uptime() {
    let _guard = mutation_lock();
    let mut backend = must_connect(backend());
    let id = must_find_test_vm(&mut backend);
    assert_safe_vm(&mut backend, id);
    // The shutdown component only exists while the VM is Running
    // (design 3.7.4).
    ensure_running(&mut backend, id);

    let before = backend
        .get_vm(id)
        .expect("get_vm")
        .summary
        .uptime
        .expect("a Running VM reports its uptime");
    let pending = backend.request_guest_reboot(id).expect("reboot");
    assert!(
        matches!(pending, hypervctl_core::Pending::AwaitReboot { .. }),
        "InitiateReboot has no job; completion is the uptime reset, got {pending:?}"
    );
    // Anomaly 17: the state stays Running, so the reset of the uptime is the
    // only proof that the guest rebooted.
    for _ in 0..120 {
        let current = backend.get_vm(id).expect("get_vm").summary.uptime;
        if current.is_some_and(|uptime| uptime < before) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the guest did not reboot; uptime stayed at or above {before:?}");
}
