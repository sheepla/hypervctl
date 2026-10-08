//! The fixture shipped with the mock: a handful of VMs in different states
//! (including an unknown one and two with the same name), a checkpoint tree,
//! switches and one VHD (design 3.5).

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use hypervctl_core::{
    Checkpoint, CheckpointId, CheckpointKind, Controller, ControllerKind, IntegrationService,
    IntegrationStatus, MemoryConfig, NicConfig, SwitchDetail, SwitchId, SwitchKind, SwitchSummary,
    VhdAttachment, VhdFormat, VhdInfo, VhdKind, VlanSettings, VmDetail, VmId, VmState, VmSummary,
};
use uuid::Uuid;

use crate::{CheckpointEntry, MockBackend};

const GIB: u64 = 1024 * 1024 * 1024;

fn id(n: u128) -> VmId {
    VmId::new(Uuid::from_u128(n))
}

fn vm(
    n: u128,
    name: &str,
    state: VmState,
    generation: u8,
    processors: u32,
    memory_gib: u64,
    checkpoint_count: u32,
) -> VmDetail {
    let running = state == VmState::Running;
    let startup = memory_gib * GIB;
    VmDetail {
        summary: VmSummary {
            id: id(n),
            name: name.to_string(),
            state,
            cpu_usage_percent: if running { Some(3.5) } else { None },
            memory_assigned_bytes: Some(startup),
            uptime: if running {
                Some(Duration::from_secs(3600))
            } else {
                None
            },
            generation: Some(generation),
        },
        processor_count: processors,
        memory: MemoryConfig {
            startup_bytes: startup,
            dynamic_enabled: true,
            min_bytes: Some(startup / 2),
            max_bytes: Some(startup * 2),
        },
        disks: Vec::new(),
        nics: if running {
            vec![NicConfig {
                switch_name: Some("test-external".to_string()),
                mac_address: Some("00:15:5d:01:02:03".to_string()),
                ip_addresses: vec![IpAddr::from([192, 168, 1, 10])],
            }]
        } else {
            Vec::new()
        },
        checkpoint_count,
        config_path: format!(r"C:\HyperV\{name}\{name}.vmcx"),
        integration_services: if running {
            vec![
                IntegrationService {
                    name: "Heartbeat".to_string(),
                    enabled: true,
                    status: IntegrationStatus::Operational,
                },
                IntegrationService {
                    name: "Time Synchronization".to_string(),
                    enabled: true,
                    status: IntegrationStatus::Operational,
                },
            ]
        } else {
            Vec::new()
        },
        notes: None,
    }
}

impl MockBackend {
    /// The backend with the default fixture loaded (design 3.5).
    pub fn with_fixture() -> Self {
        let mut backend = Self::new();

        backend.vms = vec![
            vm(1, "web-01", VmState::Running, 2, 2, 2, 2),
            vm(2, "web-02", VmState::Off, 2, 2, 2, 0),
            vm(3, "db-01", VmState::Saved, 1, 4, 4, 0),
            // A state the backend could not map: kept raw instead of dropped.
            vm(4, "ghost", VmState::Unknown(42), 1, 1, 1, 0),
            // Two VMs with the same name, for the Ambiguous case.
            vm(5, "dup", VmState::Running, 2, 2, 1, 0),
            vm(6, "dup", VmState::Off, 2, 2, 1, 0),
        ];

        let root = CheckpointId::new(Uuid::from_u128(0x100));
        let child = CheckpointId::new(Uuid::from_u128(0x101));
        backend.checkpoints = vec![
            CheckpointEntry {
                vm: id(1),
                data: Checkpoint {
                    id: root,
                    name: "before-upgrade".to_string(),
                    kind: CheckpointKind::Standard,
                    created: Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                    parent_id: None,
                },
            },
            CheckpointEntry {
                vm: id(1),
                data: Checkpoint {
                    id: child,
                    name: "nightly".to_string(),
                    kind: CheckpointKind::Standard,
                    created: Some(UNIX_EPOCH + Duration::from_secs(1_700_086_400)),
                    parent_id: Some(root),
                },
            },
        ];

        backend.switches = vec![
            SwitchDetail {
                summary: SwitchSummary {
                    id: SwitchId::new(Uuid::from_u128(0x200)),
                    name: "test-external".to_string(),
                    kind: SwitchKind::External,
                    uplink: Some("Ethernet".to_string()),
                },
                management_os_shared: false,
                vlans: VlanSettings::default(),
                connected_vm_ids: vec![id(1), id(5)],
            },
            SwitchDetail {
                summary: SwitchSummary {
                    id: SwitchId::new(Uuid::from_u128(0x201)),
                    name: "test-priv".to_string(),
                    kind: SwitchKind::Private,
                    uplink: None,
                },
                management_os_shared: true,
                vlans: VlanSettings::default(),
                connected_vm_ids: Vec::new(),
            },
        ];

        let disk_path = PathBuf::from(r"C:\HyperV\web-01\disk.vhdx");
        backend.vhds = vec![VhdInfo {
            path: disk_path.clone(),
            format: VhdFormat::Vhdx,
            kind: VhdKind::Dynamic,
            virtual_size_bytes: 64 * GIB,
            file_size_bytes: 4 * GIB,
            block_size_bytes: Some(32 * 1024 * 1024),
            parent_path: None,
            disk_id: None,
        }];
        let attachment = VhdAttachment {
            vm_id: id(1),
            vm_name: "web-01".to_string(),
            controller: Controller {
                kind: ControllerKind::Scsi,
                number: 0,
                lun: 0,
            },
            path: disk_path,
        };
        // `vm get` and `vhd list` must agree about the attachment.
        if let Some(first) = backend.vms.first_mut() {
            first.disks.push(attachment.clone());
        }
        backend.attachments = vec![attachment];

        backend
    }
}
