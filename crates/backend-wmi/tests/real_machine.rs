//! Real-Hyper-V acceptance tests for the read-only WMI backend (M6).
//!
//! Every test is `#[ignore]`d: they need a Windows host with Hyper-V and a
//! non-empty VM inventory, so they never run on CI. Run them with:
//!
//! ```text
//! cargo test -p hypervctl-backend-wmi --test real_machine -- --ignored
//! ```
//!
//! Design 6.2 applies: never create, start, stop, save, pause or delete
//! anything. These tests only read.

#![cfg(windows)]

use hypervctl_backend_wmi::WmiBackend;
use hypervctl_core::{HyperV, TargetKind, VmState};

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
    use hypervctl_core::VmId;
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
