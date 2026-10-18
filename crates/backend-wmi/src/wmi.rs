//! WMI-backed implementation of the Hyper-V backend (design 3.1, 3.7).
//!
//! Reads and mutations both live here. Mutations follow M0's measured
//! patterns: `RequestStateChange` for power, `Msvm_VirtualSystemSnapshotService`
//! for checkpoints, and out-params as `HashMap<String, Variant>`. The
//! connection is created on the actor thread because WMI objects are not
//! `Send` (design 3.2).

use std::path::PathBuf;
use std::time::Duration;

use hypervctl_core::{
    AttachedVhd, ChangeEvent, Checkpoint, CheckpointId, CheckpointKind, Controller, ControllerKind,
    HyperV, HyperVError, JobFailReason, JobRef, JobStatus, MemoryConfig, NicConfig, Pending,
    RawError, RequestedState, SwitchDetail, SwitchId, SwitchKind, SwitchSummary, TargetKind,
    Thumbnail, VhdAttachment, VhdFormat, VhdInfo, VhdKind, VlanMode, VlanSettings, VmDetail, VmId,
    VmState, VmSummary,
};
use serde::Deserialize;
use tokio::sync::mpsc;
use wmi::{IWbemClassWrapper, Variant, WMIConnection};

use crate::error::classify_hresult;

/// The Hyper-V WMI namespace (design 3.7).
const NAMESPACE: &str = r"root\virtualization\v2";

/// Handle to a WMI connection against `root\virtualization\v2`.
///
/// Created on the actor thread; never moved off it (design 3.2).
pub struct WmiBackend {
    state: State,
    /// Counts WMI round-trips so tests can prove `list_vms` is not N+1.
    #[cfg(test)]
    query_count: u32,
    /// Counts WMI method calls so tests can pin `list_vhds`' cost to the
    /// formula in design 3.8.3 (M11-W2).
    #[cfg(test)]
    method_count: u32,
}

enum State {
    Connected(WMIConnection),
    /// The connection could not be opened; every call returns this error.
    Unavailable(HyperVError),
}

impl std::fmt::Debug for WmiBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.state {
            State::Connected(_) => f.debug_struct("WmiBackend").finish_non_exhaustive(),
            State::Unavailable(error) => f
                .debug_struct("WmiBackend")
                .field("unavailable", error)
                .finish(),
        }
    }
}

/// A row of `Msvm_SummaryInformation`. Only the columns the frontend needs are
/// selected; `Snapshots` is known to fail deserialization and `__Path` breaks
/// the row entirely (M0 items 1–2), so neither appears in the query.
#[derive(Debug, Deserialize)]
struct SummaryRow {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "ElementName")]
    element_name: String,
    #[serde(rename = "EnabledState")]
    enabled_state: u32,
    #[serde(rename = "MemoryUsage")]
    memory_usage: Option<u64>,
    #[serde(rename = "UpTime")]
    up_time: Option<u64>,
    /// Hyper-V reports this as a CIM `uint16`; selected in both queries.
    #[serde(rename = "NumberOfProcessors")]
    number_of_processors: Option<u16>,
    /// Percentage (0–100); absent for VMs that are not running.
    #[serde(rename = "ProcessorLoad")]
    processor_load: Option<u16>,
}

/// A row of `Msvm_ComputerSystem`, used to name the disks `vhd list` returns.
/// The host also appears here, with a non-GUID `Name`; it is filtered out when
/// the name is parsed as a VM id.
#[derive(Debug, Deserialize)]
struct VmNameRow {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "ElementName")]
    element_name: String,
}

/// A row of `Msvm_VirtualSystemSettingData`, used by `list_vms` to read each
/// VM's generation without a per-VM query. The snapshot rows are skipped.
#[derive(Debug, Deserialize)]
struct VssdGenerationRow {
    #[serde(rename = "VirtualSystemIdentifier")]
    virtual_system_identifier: Option<String>,
    #[serde(rename = "VirtualSystemType")]
    virtual_system_type: Option<String>,
    #[serde(rename = "VirtualSystemSubType")]
    virtual_system_sub_type: Option<String>,
}

/// A row of `Msvm_VirtualSystemSettingData`.
#[derive(Debug, Deserialize)]
struct VssdRow {
    #[serde(rename = "InstanceID")]
    instance_id: String,
    #[serde(rename = "ElementName")]
    element_name: Option<String>,
    #[serde(rename = "VirtualSystemType")]
    virtual_system_type: Option<String>,
    #[serde(rename = "Parent")]
    parent: Option<String>,
    #[serde(rename = "CreationTime")]
    creation_time: Option<String>,
    #[serde(rename = "UserSnapshotType")]
    user_snapshot_type: Option<u32>,
    #[serde(rename = "VirtualSystemSubType")]
    virtual_system_sub_type: Option<String>,
    #[serde(rename = "ConfigurationDataRoot")]
    configuration_data_root: Option<String>,
    #[serde(rename = "ConfigurationFile")]
    configuration_file: Option<String>,
    /// `Notes` is a string *array* on real Hyper-V (M0 probe observation).
    #[serde(rename = "Notes")]
    notes: Option<Vec<String>>,
}

/// A row of `Msvm_VirtualSwitch`.
#[derive(Debug, Deserialize)]
struct SwitchRow {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "ElementName")]
    element_name: String,
}

/// A row of `Msvm_ConcreteJob` for [`HyperV::job_status`].
#[derive(Debug, Deserialize)]
struct JobRow {
    #[serde(rename = "JobState")]
    job_state: u32,
    #[serde(rename = "ErrorCode")]
    error_code: Option<u32>,
    #[serde(rename = "PercentComplete")]
    percent_complete: Option<u32>,
    #[serde(rename = "ErrorDescription")]
    error_description: Option<String>,
}

/// A row of `Msvm_StorageAllocationSettingData` (one virtual disk attachment).
/// `InstanceID` is `Microsoft:{vm-guid}\{controller-guid}\{controller-num}\{address}\L`.
/// `ResourceSubType` distinguishes a hard disk from a CD/DVD or floppy
/// attachment (M11-W2).
#[derive(Debug, Deserialize)]
struct SdrdRow {
    #[serde(rename = "InstanceID")]
    instance_id: String,
    #[serde(rename = "ResourceSubType")]
    resource_sub_type: Option<String>,
    #[serde(rename = "HostResource")]
    host_resource: Option<Vec<String>>,
}

/// A row of `Msvm_ResourceAllocationSettingData` used to classify a controller.
#[derive(Debug, Deserialize)]
struct ControllerRasdRow {
    #[serde(rename = "InstanceID")]
    instance_id: String,
    #[serde(rename = "ResourceType")]
    resource_type: Option<u16>,
}

/// A row of `Msvm_SyntheticEthernetPortSettingData` (NIC MAC address).
/// `InstanceID` is `Microsoft:{vm-guid}\{nic-guid}`.
#[derive(Debug, Deserialize)]
struct SespdRow {
    #[serde(rename = "InstanceID")]
    instance_id: String,
    #[serde(rename = "Address")]
    address: Option<String>,
}

/// A row of `Msvm_MemorySettingData` for a VM. The `Default` entry carries
/// the startup amount and the dynamic-memory bounds; `VirtualQuantity`,
/// `Reservation` and `Limit` are in MB (`VirtualQuantityUnits`).
#[derive(Debug, Deserialize)]
struct MemorySettingDataRow {
    #[serde(rename = "VirtualQuantity")]
    virtual_quantity: Option<u64>,
    #[serde(rename = "DynamicMemoryEnabled")]
    dynamic_memory_enabled: Option<bool>,
    #[serde(rename = "Limit")]
    limit: Option<u64>,
    #[serde(rename = "Reservation")]
    reservation: Option<u64>,
}

/// A row of `Msvm_EthernetPortAllocationSettingData` (NIC switch connection).
/// `InstanceID` is `Microsoft:{vm-guid}\{nic-guid}\C`.
#[derive(Debug, Deserialize)]
struct EpasdRow {
    #[serde(rename = "InstanceID")]
    instance_id: String,
    #[serde(rename = "LastKnownSwitchName")]
    last_known_switch_name: Option<String>,
}

impl WmiBackend {
    /// Opens a connection to `root\virtualization\v2`. Never panics: a failure
    /// is remembered and returned from every subsequent call.
    pub fn connect_or_unavailable() -> Self {
        match WMIConnection::with_namespace_path(NAMESPACE) {
            Ok(con) => Self {
                state: State::Connected(con),
                #[cfg(test)]
                query_count: 0,
                #[cfg(test)]
                method_count: 0,
            },
            Err(error) => Self {
                state: State::Unavailable(map_wmi_error(error)),
                #[cfg(test)]
                query_count: 0,
                #[cfg(test)]
                method_count: 0,
            },
        }
    }

    fn run_query<T: serde::de::DeserializeOwned>(
        &mut self,
        wql: &str,
    ) -> Result<Vec<T>, HyperVError> {
        #[cfg(test)]
        {
            self.query_count += 1;
        }
        let con = match &self.state {
            State::Connected(con) => con,
            State::Unavailable(error) => return Err(error.clone()),
        };
        con.raw_query(wql).map_err(map_wmi_error)
    }

    /// Borrows the live connection, or replays the remembered failure.
    fn connected(&mut self) -> Result<&WMIConnection, HyperVError> {
        match &self.state {
            State::Connected(con) => Ok(con),
            State::Unavailable(error) => Err(error.clone()),
        }
    }

    /// [`exec_method_with`] plus a test-only call counter, so `wmi_round_trips`
    /// can pin the cost of `list_vhds` (M11-W2).
    fn exec_method_counted(
        &mut self,
        object_path: &str,
        class_name: &str,
        method_name: &str,
        props: &[(&str, Variant)],
    ) -> Result<IWbemClassWrapper, HyperVError> {
        #[cfg(test)]
        {
            self.method_count += 1;
        }
        let con = self.connected()?;
        exec_method_with(con, object_path, class_name, method_name, props)
    }

    /// Reads a VHD's metadata through `Msvm_ImageManagementService` (design
    /// 3.8.3). Two calls per disk: the setting data (format, type, virtual and
    /// block size, parent, disk id) and the state (file size). `None` when the
    /// service cannot report the disk, e.g. the file is gone or the path is not
    /// a virtual hard disk.
    fn fetch_vhd_info(&mut self, path: &str) -> Option<VhdInfo> {
        let host = host_name();
        let service = image_management_service_path(&host);
        let setting = match self.exec_method_counted(
            &service,
            "Msvm_ImageManagementService",
            "GetVirtualHardDiskSettingData",
            &[("Path", Variant::String(path.to_string()))],
        ) {
            Ok(out) => method_embedded_text(&out, "SettingData"),
            Err(error) => {
                tracing::debug!("GetVirtualHardDiskSettingData({path}) failed: {error}");
                None
            }
        };
        let setting = setting?;
        let state = match self.exec_method_counted(
            &service,
            "Msvm_ImageManagementService",
            "GetVirtualHardDiskState",
            &[("Path", Variant::String(path.to_string()))],
        ) {
            Ok(out) => method_embedded_text(&out, "State"),
            Err(error) => {
                tracing::debug!("GetVirtualHardDiskState({path}) failed: {error}");
                None
            }
        };
        vhd_info_from_instances(path, &setting, state.as_deref())
    }

    fn summary_rows(&mut self) -> Result<Vec<SummaryRow>, HyperVError> {
        // One query for every VM (design 3.1: N+1 forbidden). `Snapshots` and
        // `__Path` are left out on purpose (M0 items 1–2).
        self.run_query(
            "SELECT Name, ElementName, EnabledState, MemoryUsage, UpTime, NumberOfProcessors, ProcessorLoad \
             FROM Msvm_SummaryInformation",
        )
    }

    fn summary_row_for(&mut self, id: VmId) -> Result<Option<SummaryRow>, HyperVError> {
        let wql = format!(
            "SELECT Name, ElementName, EnabledState, MemoryUsage, UpTime, NumberOfProcessors, ProcessorLoad \
             FROM Msvm_SummaryInformation WHERE Name = '{id}'"
        );
        let mut rows = self.run_query::<SummaryRow>(&wql)?;
        Ok(rows.pop())
    }

    fn vssd_rows_for_vm(&mut self, id: VmId) -> Result<Vec<VssdRow>, HyperVError> {
        let wql = format!(
            "SELECT InstanceID, ElementName, VirtualSystemType, Parent, \
                    CreationTime, UserSnapshotType, VirtualSystemSubType, ConfigurationDataRoot, \
                    ConfigurationFile, Notes \
             FROM Msvm_VirtualSystemSettingData WHERE VirtualSystemIdentifier = '{id}'"
        );
        self.run_query(&wql)
    }

    /// The memory setting of a VM, if Hyper-V reported one. The entry whose
    /// InstanceID starts with `Microsoft:{vm-guid}\` carries the startup
    /// amount and the dynamic-memory bounds.
    fn memory_setting_for_vm(
        &mut self,
        id: VmId,
    ) -> Result<Option<MemorySettingDataRow>, HyperVError> {
        let wql = format!(
            "SELECT VirtualQuantity, DynamicMemoryEnabled, Limit, Reservation \
             FROM Msvm_MemorySettingData WHERE InstanceID LIKE 'Microsoft:{id}\\\\%'"
        );
        let mut rows = self.run_query::<MemorySettingDataRow>(&wql)?;
        Ok(rows.pop())
    }

    /// The realized (non-snapshot) VSSD of a VM, if Hyper-V reported one.
    fn realized_vssd(rows: &[VssdRow]) -> Option<&VssdRow> {
        rows.iter()
            .find(|row| is_active_config(row.virtual_system_type.as_deref()))
    }

    /// NICs of a VM: MAC from SESPD, switch name from EPASD. Both share the
    /// InstanceID prefix `Microsoft:{vm-guid}\{nic-guid}`.
    fn nics_for_vm(&mut self, id: VmId) -> Result<Vec<NicConfig>, HyperVError> {
        // WQL treats `\` as an escape; `\\\\` in Rust becomes `\\` in the query,
        // which WQL reads as a literal backslash.
        let sespd: Vec<SespdRow> = self.run_query(&format!(
            "SELECT InstanceID, Address FROM Msvm_SyntheticEthernetPortSettingData \
             WHERE InstanceID LIKE 'Microsoft:{id}\\\\%'"
        ))?;
        let epasd: Vec<EpasdRow> = self.run_query(&format!(
            "SELECT InstanceID, LastKnownSwitchName FROM Msvm_EthernetPortAllocationSettingData \
             WHERE InstanceID LIKE 'Microsoft:{id}\\\\%'"
        ))?;
        // EPASD InstanceID is `...{nic-guid}\C`; strip the trailing `\C` to join.
        let switch_map: std::collections::HashMap<String, Option<String>> = epasd
            .into_iter()
            .map(|row| {
                let key = row
                    .instance_id
                    .strip_suffix("\\C")
                    .unwrap_or(&row.instance_id)
                    .to_string();
                (key, row.last_known_switch_name.filter(|s| !s.is_empty()))
            })
            .collect();
        Ok(sespd
            .into_iter()
            .map(|row| NicConfig {
                switch_name: switch_map.get(&row.instance_id).cloned().flatten(),
                // Hyper-V reports MAC as bare hex; insert colons for display.
                mac_address: row.address.filter(|a| !a.is_empty()).map(|a| {
                    a.as_bytes()
                        .chunks(2)
                        .map(|pair| {
                            format!(
                                "{:02X}",
                                u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16)
                                    .unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(":")
                }),
            })
            .collect())
    }
}

fn map_wmi_error(error: wmi::WMIError) -> HyperVError {
    match error {
        wmi::WMIError::HResultError { hres } => classify_hresult(hres),
        other => HyperVError::backend(RawError::new(other.to_string())),
    }
}

fn parse_vm_id(name: &str) -> Result<VmId, HyperVError> {
    name.parse::<VmId>().map_err(|error| {
        HyperVError::backend(RawError::new(format!(
            "WMI reported a VM name that is not a GUID: {name} ({error})"
        )))
    })
}

/// Converts one summary row into the domain type. Pure so it is unit-tested
/// without a WMI connection (M0 item 2 units and Off semantics).
fn summary_from_row(row: SummaryRow) -> Result<VmSummary, HyperVError> {
    let id = parse_vm_id(&row.name)?;
    let state = VmState::from_enabled_state(row.enabled_state);
    // Off/Saved VMs report no CPU usage and no uptime (spec 2.10).
    // `ProcessorLoad` is a percentage and is absent when the VM is not running.
    let cpu_usage_percent = match state {
        VmState::Off | VmState::Saved => None,
        _ => row.processor_load.map(f64::from),
    };
    let memory_assigned_bytes = row.memory_usage.map(|mb| mb * 1024 * 1024);
    let uptime = match state {
        VmState::Off | VmState::Saved => None,
        _ => row.up_time.filter(|ms| *ms > 0).map(Duration::from_millis),
    };
    // Generation is not on SummaryInformation; get_vm fills it in.
    Ok(VmSummary {
        id,
        name: row.element_name,
        state,
        cpu_usage_percent,
        memory_assigned_bytes,
        uptime,
        generation: None,
    })
}

/// Maps `UserSnapshotType` (M0 item 6): Standard=5, Production=3,
/// ProductionOnly=4. Hyper-V reports "production only" as Production here.
fn checkpoint_kind(user_snapshot_type: Option<u32>) -> CheckpointKind {
    match user_snapshot_type {
        Some(5) => CheckpointKind::Standard,
        Some(3) | Some(4) => CheckpointKind::Production,
        Some(other) => CheckpointKind::Unknown(other),
        None => CheckpointKind::Unknown(0),
    }
}

/// Parses a DMTF timestamp `yyyymmddHHMMSS.mmmmmm-000` into a `SystemTime`.
/// Returns `None` when the shape is wrong; the caller keeps `None` (design).
fn parse_dmtf_time(raw: &str) -> Option<std::time::SystemTime> {
    let digits: String = raw.chars().take(14).filter(char::is_ascii_digit).collect();
    if digits.len() < 14 {
        return None;
    }
    let (year, rest) = digits.split_at(4);
    let (month, rest) = rest.split_at(2);
    let (day, rest) = rest.split_at(2);
    let (hour, rest) = rest.split_at(2);
    let (minute, second) = rest.split_at(2);
    let parse = |s: &str| s.parse::<u32>().ok();
    let (y, mo, d, h, mi, se) = (
        parse(year)?,
        parse(month)?,
        parse(day)?,
        parse(hour)?,
        parse(minute)?,
        parse(second)?,
    );
    // Days per month; February is treated as 28 (leap days are ignored rather
    // than guessed wrong).
    let days_in_month = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28,
        _ => return None,
    };
    if mo == 0 || mo > 12 || d == 0 || d > days_in_month || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + i64::from(h) * 3_600 + i64::from(mi) * 60 + i64::from(se);
    Some(std::time::UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: u32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = ((153 * (i64::from(m) + if m > 2 { -3 } else { 9 }) + 2) / 5) + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Builds the WMI object path for a VM (M0 item 1: composite key, braces around
/// the GUID; never obtained by projecting `__Path`).
fn vm_object_path(id: VmId) -> String {
    format!(
        "\\\\.\\{NAMESPACE}:Msvm_ComputerSystem.CreationClassName=\"Msvm_ComputerSystem\",Name=\"{id}\""
    )
}

/// The checkpoint id embedded in an `InstanceID` of the form `Microsoft:<guid>`.
fn checkpoint_id_from_instance(instance_id: &str) -> Option<CheckpointId> {
    let guid = instance_id.strip_prefix("Microsoft:")?;
    guid.parse::<uuid::Uuid>().ok().map(CheckpointId::new)
}

/// Whether a VSSD is a VM's active configuration
/// (`Microsoft:Hyper-V:System:Realized`) rather than a checkpoint
/// (`Microsoft:Hyper-V:Snapshot:Realized`).
fn is_active_config(virtual_system_type: Option<&str>) -> bool {
    virtual_system_type.is_some_and(|t| t.starts_with("Microsoft:Hyper-V:System"))
}

/// `VirtualSystemSubType` ends with `:1` (Gen1) or `:2` (Gen2) (M0 item 6).
fn generation_from_subtype(subtype: Option<&str>) -> Option<u8> {
    let tail = subtype?.rsplit(':').next()?;
    match tail {
        "1" => Some(1),
        "2" => Some(2),
        _ => None,
    }
}

/// `Msvm_VirtualSwitch.SwitchType` is not a WMI property; Hyper-V exposes the
/// kind through associations. Until M7 walks them the kind stays unknown.
fn switch_kind(_raw: Option<u32>) -> SwitchKind {
    SwitchKind::Unknown(0)
}

/// CIM `uint16` values accepted by `Msvm_ComputerSystem.RequestStateChange`
/// (M0 item 5). `Resume` and `Start` share value 2 (Enabled).
fn requested_state_value(request: RequestedState) -> u16 {
    match request {
        RequestedState::Start | RequestedState::Resume => 2,
        RequestedState::TurnOff => 3,
        RequestedState::ShutDown => 4,
        RequestedState::Save => 6,
        RequestedState::Pause => 9,
        RequestedState::Reset => 11,
    }
}

/// Builds the WMI object path for the snapshot service (M0 item 6: four-key
/// composite; `SystemName` is the host name).
fn snapshot_service_path(host: &str) -> String {
    format!(
        "\\\\.\\{NAMESPACE}:Msvm_VirtualSystemSnapshotService.CreationClassName=\"Msvm_VirtualSystemSnapshotService\",Name=\"vssnapsvc\",SystemCreationClassName=\"Msvm_ComputerSystem\",SystemName=\"{host}\""
    )
}

/// Builds the WMI object path for the virtual system management service, the
/// singleton that owns `GetVirtualSystemThumbnailImage` (M13). Four-key
/// composite like the snapshot service; the singleton's `Name` is `vmms`.
fn management_service_path(host: &str) -> String {
    format!(
        "\\\\.\\{NAMESPACE}:Msvm_VirtualSystemManagementService.CreationClassName=\"Msvm_VirtualSystemManagementService\",Name=\"vmms\",SystemCreationClassName=\"Msvm_ComputerSystem\",SystemName=\"{host}\""
    )
}

/// Builds the WMI object path for the image management service, the singleton
/// that owns the VHD read methods (design 3.8.3). Four-key composite; unlike
/// the management service its singleton `Name` is `vhdsvc` (M11-V).
fn image_management_service_path(host: &str) -> String {
    format!(
        "\\\\.\\{NAMESPACE}:Msvm_ImageManagementService.CreationClassName=\"Msvm_ImageManagementService\",Name=\"vhdsvc\",SystemCreationClassName=\"Msvm_ComputerSystem\",SystemName=\"{host}\""
    )
}

/// Strips the four-byte big-endian length prefix Hyper-V puts in front of the
/// pixels and decodes the rest as RGB565 (M13-V:
/// `docs/hypervctl-m13-findings.md`).
fn decode_thumbnail(width: u16, height: u16, data: &[u8]) -> Option<Thumbnail> {
    let pixels = data.get(4..)?;
    Thumbnail::from_rgb565(u32::from(width), u32::from(height), pixels)
}

/// Builds the WMI object path for a VM's shutdown component (design 3.3,
/// `Msvm_ShutdownComponent`). Four-key composite, like the snapshot service.
fn shutdown_component_path(host: &str, vm: VmId) -> String {
    format!(
        "\\\\.\\{NAMESPACE}:Msvm_ShutdownComponent.CreationClassName=\"Msvm_ShutdownComponent\",Name=\"{vm}\",SystemCreationClassName=\"Msvm_ComputerSystem\",SystemName=\"{host}\""
    )
}

/// Builds the WMI object path for a VSSD instance (M0 item 6: `InstanceID` is
/// the sole key of `Msvm_VirtualSystemSettingData`; adding `CreationClassName`
/// makes the path invalid with `WBEM_E_INVALID_OBJECT_PATH`).
fn vssd_object_path(instance_id: &str) -> String {
    format!("\\\\.\\{NAMESPACE}:Msvm_VirtualSystemSettingData.InstanceID=\"{instance_id}\"")
}

/// `InstanceID` of the VSSD behind a checkpoint id (`Microsoft:<guid>`).
fn vssd_instance_id(checkpoint: CheckpointId) -> String {
    format!("Microsoft:{checkpoint}")
}

/// Serializes a WMI instance to the DTD text `ModifySystemSettings` expects
/// (design 3.3, M11-V V4). `IWbemObjectTextSrc` is the only serializer the
/// provider accepts; building the text by hand is not (the checkpoint's VSSD
/// carries propagated properties and arrays).
fn vssd_to_dtd_text(vssd: &IWbemClassWrapper) -> Result<String, HyperVError> {
    use windows::core::GUID;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::System::Wmi::{IWbemObjectTextSrc, WMI_OBJ_TEXT_WMI_DTD_2_0};

    // `WbemObjectTextSrc` is not exported as a named constant by the `windows`
    // crate; this is its CLSID.
    const CLSID_WBEM_OBJECT_TEXT_SRC: GUID =
        GUID::from_u128(0x8d1c559d_84f0_4bb3_a7d5_56a7435a9ba6);
    let source: IWbemObjectTextSrc =
        unsafe { CoCreateInstance(&CLSID_WBEM_OBJECT_TEXT_SRC, None, CLSCTX_INPROC_SERVER) }
            .map_err(|error| {
                HyperVError::backend(RawError::new(format!(
                    "CoCreateInstance(WbemObjectTextSrc) failed: {error}"
                )))
            })?;
    let text = unsafe { source.GetText(0, &vssd.inner, WMI_OBJ_TEXT_WMI_DTD_2_0.0 as u32, None) }
        .map_err(|error| {
        HyperVError::backend(RawError::new(format!(
            "WbemObjectTextSrc.GetText failed: {error}"
        )))
    })?;
    Ok(text.to_string())
}

/// The host name Hyper-V uses as `SystemName` on service objects.
fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| String::from("localhost"))
}

/// Splits an SDRD `InstanceID` into (vm_id, controller_number, lun).
/// Format: `Microsoft:{vm-guid}\{controller-guid}\{controller-num}\{address}\L`.
/// Returns `None` for definition rows (`Microsoft:Definition\...`).
fn parse_sdrd_instance_id(instance_id: &str) -> Option<(uuid::Uuid, u8, u8)> {
    let rest = instance_id.strip_prefix("Microsoft:")?;
    let mut parts = rest.split('\\');
    let vm_guid = parts.next()?.parse::<uuid::Uuid>().ok()?;
    let _controller_guid = parts.next()?;
    let controller_number = parts.next()?.parse::<u8>().ok()?;
    let lun = parts.next()?.parse::<u8>().ok()?;
    Some((vm_guid, controller_number, lun))
}

/// Returns the `Microsoft:{vm-guid}\{controller-guid}\` prefix shared by an
/// SDRD's `InstanceID` and its controller RASD's `InstanceID`. Both GUIDs stay
/// in the case the provider reported, because `starts_with` is exact.
fn controller_key_prefix(instance_id: &str) -> Option<String> {
    let rest = instance_id.strip_prefix("Microsoft:")?;
    let mut parts = rest.split('\\');
    let vm_part = parts.next()?;
    let controller_guid = parts.next()?;
    Some(format!("Microsoft:{vm_part}\\{controller_guid}\\"))
}

/// Maps a controller RASD's `ResourceType` to [`ControllerKind`].
fn controller_kind_from_rasd(resource_type: Option<u16>) -> ControllerKind {
    match resource_type {
        Some(5) => ControllerKind::Ide,
        Some(6) => ControllerKind::Scsi,
        Some(7) => ControllerKind::Sata,
        other => ControllerKind::Unknown(other.unwrap_or(0)),
    }
}

/// Reads `ReturnValue` from a method out-params wrapper. Missing or wrong-typed
/// is reported as a backend error rather than guessed.
fn return_value(out: &IWbemClassWrapper) -> Result<u32, HyperVError> {
    match out.get_property("ReturnValue") {
        Ok(Variant::UI4(value)) => Ok(value),
        Ok(Variant::UI2(value)) => Ok(u32::from(value)),
        Ok(other) => Err(HyperVError::backend(RawError::new(format!(
            "method ReturnValue had unexpected type: {other:?}"
        )))),
        Err(error) => Err(map_wmi_error(error)),
    }
}

/// Reads the `Job` out-param (an object path string) when Hyper-V started one.
fn job_path(out: &IWbemClassWrapper) -> Option<String> {
    match out.get_property("Job") {
        Ok(Variant::String(path)) if !path.is_empty() => Some(path),
        Ok(_) => None,
        Err(_) => None,
    }
}

/// Extracts the last quoted value from a WMI object path, which for Hyper-V
/// job and VSSD paths is the `InstanceID` (M0 item 6: keys end with it).
fn instance_id_from_path(path: &str) -> Option<String> {
    let start = path.rfind("=\"")? + 2;
    let rest = path.get(start..)?;
    let end = rest.find('"')?;
    let value = rest.get(..end)?;
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Executes a WMI method with the given properties set on a spawned in-params
/// instance. Properties that are absent stay absent (M0 item 6: putting an
/// empty `SnapshotSettings` fails with `0x80041006`).
fn exec_method_with(
    con: &WMIConnection,
    object_path: &str,
    class_name: &str,
    method_name: &str,
    props: &[(&str, Variant)],
) -> Result<IWbemClassWrapper, HyperVError> {
    let method_class = con
        .get_object(class_name)
        .and_then(|class| class.get_method(method_name))
        .map_err(map_wmi_error)?
        .ok_or_else(|| {
            HyperVError::backend(RawError::new(format!(
                "WMI class {class_name} has no method {method_name}"
            )))
        })?;
    let in_params = method_class.spawn_instance().map_err(map_wmi_error)?;
    for (name, value) in props {
        in_params
            .put_property(name, value.clone())
            .map_err(map_wmi_error)?;
    }
    con.exec_method(object_path, method_name, Some(&in_params))
        .map_err(map_wmi_error)?
        .ok_or_else(|| {
            HyperVError::backend(RawError::new(format!(
                "WMI method {method_name} returned no out-params"
            )))
        })
}

/// Classifies a failed job's `ErrorCode` into the hint the spec shows
/// (design 3.8.2, spec 2.12): a pure function of the code alone, because the
/// provider's message is localized (design 3.7.6). Both observed shapes are
/// matched: the HRESULT-style value `Msvm_ConcreteJob.ErrorCode` carries
/// (design 3.7.9) and the raw codes the M0 probe recorded for the very same
/// failures (m0-findings: 32768 = guest not ready, 32778 = out of memory,
/// 32770 = unsupported). Everything unclassified keeps `Other`: only `-v`
/// explains those.
fn job_fail_reason(code: u32) -> JobFailReason {
    match code {
        0x8007_10DF | 32768 => JobFailReason::GuestNotReady,
        0x8007_000E | 32778 => JobFailReason::HostOutOfMemory,
        _ => JobFailReason::Other,
    }
}

/// Maps a `RequestStateChange` / snapshot-method result onto `Pending`.
/// `0x80004005` is never returned here; `32775` is the "already there" /
/// "not valid now" answer and becomes `InvalidState` (M0 item 5).
fn pending_from_method(out: &IWbemClassWrapper, operation: &str) -> Result<Pending, HyperVError> {
    let rv = return_value(out)?;
    match rv {
        0 => Ok(Pending::Done),
        4096 => match job_path(out) {
            Some(path) => {
                let id = instance_id_from_path(&path).unwrap_or(path);
                Ok(Pending::Job(JobRef::new(id)))
            }
            None => Err(HyperVError::backend(RawError::new(format!(
                "{operation} returned 4096 (job started) but no Job path"
            )))),
        },
        32775 => Err(HyperVError::InvalidState {
            target: None,
            state: VmState::Unknown(0),
            operation: operation.to_string(),
            raw: Some(RawError::new(
                "ReturnValue=32775 (state conflict / already in target state)",
            )),
        }),
        other => Err(HyperVError::backend(RawError::new(format!(
            "{operation} returned unexpected ReturnValue={other}"
        )))),
    }
}

impl HyperV for WmiBackend {
    fn list_vms(&mut self) -> Result<Vec<VmSummary>, HyperVError> {
        let rows = self.summary_rows()?;
        let mut vms: Vec<VmSummary> = rows
            .into_iter()
            .map(summary_from_row)
            .collect::<Result<_, _>>()?;
        // Generation lives on the active VSSD, not on `Msvm_SummaryInformation`.
        // One bulk query keeps this constant in the number of VMs (design 3.1).
        let subtype_rows: Vec<VssdGenerationRow> = self.run_query(
            "SELECT VirtualSystemIdentifier, VirtualSystemType, VirtualSystemSubType \
             FROM Msvm_VirtualSystemSettingData",
        )?;
        let mut generations: std::collections::HashMap<uuid::Uuid, u8> =
            std::collections::HashMap::new();
        for row in subtype_rows {
            if !is_active_config(row.virtual_system_type.as_deref()) {
                continue;
            }
            let Some(vm_id) = row
                .virtual_system_identifier
                .as_deref()
                .and_then(|s| s.parse::<uuid::Uuid>().ok())
            else {
                continue;
            };
            if let Some(generation) =
                generation_from_subtype(row.virtual_system_sub_type.as_deref())
            {
                generations.insert(vm_id, generation);
            }
        }
        for vm in &mut vms {
            if let Some(generation) = generations.get(vm.id.as_uuid()) {
                vm.generation = Some(*generation);
            }
        }
        Ok(vms)
    }

    fn get_vm(&mut self, id: VmId) -> Result<VmDetail, HyperVError> {
        let row = self
            .summary_row_for(id)?
            .ok_or_else(|| HyperVError::NotFound {
                kind: TargetKind::Vm,
                specified: id.to_string(),
                raw: None,
            })?;
        let processor_count = row.number_of_processors.unwrap_or(0);
        let mut summary = summary_from_row(row)?;
        let vssds = self.vssd_rows_for_vm(id)?;
        if let Some(realized) = Self::realized_vssd(&vssds) {
            summary.generation =
                generation_from_subtype(realized.virtual_system_sub_type.as_deref());
        }
        let checkpoint_count = vssds
            .iter()
            .filter(|row| {
                row.virtual_system_type
                    .as_deref()
                    .is_some_and(|t| t.starts_with("Microsoft:Hyper-V:Snapshot"))
            })
            .count() as u32;
        let config_path = Self::realized_vssd(&vssds)
            .and_then(
                |row| match (&row.configuration_data_root, &row.configuration_file) {
                    (Some(root), Some(file)) if !root.is_empty() && !file.is_empty() => {
                        Some(format!(r"{root}\{file}"))
                    }
                    (root, file) => root
                        .clone()
                        .filter(|s| !s.is_empty())
                        .or_else(|| file.clone().filter(|s| !s.is_empty())),
                },
            )
            .unwrap_or_default();
        let notes = Self::realized_vssd(&vssds)
            .and_then(|row| row.notes.clone())
            .map(|lines| lines.join("\n"))
            .filter(|n| !n.is_empty());
        // Disks come from the SDRD walk (same as list_vhds, filtered to this VM).
        let disks: Vec<VhdAttachment> = self
            .list_vhds(Some(id))?
            .into_iter()
            .map(|attached| attached.attachment)
            .collect();
        let nics = self.nics_for_vm(id)?;
        // Memory bounds come from the `Default` Msvm_MemorySettingData entry;
        // quantities are in MB and converted to bytes.
        let memory = match self.memory_setting_for_vm(id)? {
            Some(row) => {
                let mb = 1024u64 * 1024;
                MemoryConfig {
                    startup_bytes: row.virtual_quantity.unwrap_or(0) * mb,
                    dynamic_enabled: row.dynamic_memory_enabled.unwrap_or(false),
                    min_bytes: row.reservation.map(|v| v * mb),
                    max_bytes: row.limit.map(|v| v * mb),
                }
            }
            None => MemoryConfig {
                startup_bytes: summary.memory_assigned_bytes.unwrap_or(0),
                dynamic_enabled: false,
                min_bytes: None,
                max_bytes: None,
            },
        };
        Ok(VmDetail {
            summary,
            processor_count: u32::from(processor_count),
            memory,
            disks,
            nics,
            checkpoint_count,
            config_path,
            // Not yet obtainable: the classes and their existence conditions
            // are pending M11-V (design 3.8.5), so this is `None` rather than
            // an empty list that would claim there are no services.
            integration_services: None,
            notes,
        })
    }

    fn list_checkpoints(&mut self, vm: VmId) -> Result<Vec<Checkpoint>, HyperVError> {
        // Ensure the VM exists so a bad id is NotFound, not an empty list.
        if self.summary_row_for(vm)?.is_none() {
            return Err(HyperVError::NotFound {
                kind: TargetKind::Vm,
                specified: vm.to_string(),
                raw: None,
            });
        }
        let rows = self.vssd_rows_for_vm(vm)?;
        let mut checkpoints: Vec<Checkpoint> = rows
            .into_iter()
            .filter(|row| {
                row.virtual_system_type
                    .as_deref()
                    .is_some_and(|t| t.starts_with("Microsoft:Hyper-V:Snapshot"))
            })
            .filter_map(|row| {
                let id = checkpoint_id_from_instance(&row.instance_id)?;
                // ElementName is the checkpoint name; fall back to InstanceID.
                let name = row
                    .element_name
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| row.instance_id.clone());
                Some(Checkpoint {
                    id,
                    name,
                    kind: checkpoint_kind(row.user_snapshot_type),
                    created: row.creation_time.as_deref().and_then(parse_dmtf_time),
                    parent_id: row
                        .parent
                        .as_deref()
                        .and_then(|p| p.split('=').nth(1))
                        .and_then(|p| p.trim_matches('"').parse::<uuid::Uuid>().ok())
                        .map(CheckpointId::new),
                })
            })
            .collect();
        checkpoints.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.name.cmp(&b.name)));
        Ok(checkpoints)
    }

    fn list_switches(&mut self) -> Result<Vec<SwitchSummary>, HyperVError> {
        // `SwitchType` is not a WMI property on this class; only identity is.
        let rows: Vec<SwitchRow> = match self
            .run_query("SELECT Name, ElementName FROM Msvm_VirtualEthernetSwitch")
        {
            Ok(rows) => rows,
            // Some hosts do not register the class until a switch exists.
            Err(HyperVError::Backend { raw }) if raw.hresult == Some(0x8004_1010_u32 as i32) => {
                return Ok(Vec::new())
            }
            Err(error) => return Err(error),
        };
        rows.into_iter()
            .map(|row| {
                let id = row
                    .name
                    .parse::<uuid::Uuid>()
                    .map(SwitchId::new)
                    .map_err(|error| {
                        HyperVError::backend(RawError::new(format!(
                            "WMI reported a switch name that is not a GUID: {} ({error})",
                            row.name
                        )))
                    })?;
                Ok(SwitchSummary {
                    id,
                    name: row.element_name,
                    kind: switch_kind(None),
                    // The uplink NIC lives on the association, not the switch row.
                    uplink: None,
                })
            })
            .collect()
    }

    fn get_switch(&mut self, id: SwitchId) -> Result<SwitchDetail, HyperVError> {
        let wql =
            format!("SELECT Name, ElementName FROM Msvm_VirtualEthernetSwitch WHERE Name = '{id}'");
        let mut rows: Vec<SwitchRow> = self.run_query(&wql)?;
        let row = rows.pop().ok_or_else(|| HyperVError::NotFound {
            kind: TargetKind::Switch,
            specified: id.to_string(),
            raw: None,
        })?;
        // Connected VMs: EPASD entries whose LastKnownSwitchName matches and
        // whose InstanceID has the `\C` suffix (VM NICs, not host vNICs).
        let epasd: Vec<EpasdRow> = self.run_query(&format!(
            "SELECT InstanceID, LastKnownSwitchName FROM Msvm_EthernetPortAllocationSettingData \
             WHERE LastKnownSwitchName = '{}'",
            row.element_name
        ))?;
        let mut connected_vm_ids = Vec::new();
        for entry in epasd {
            if entry.last_known_switch_name.as_deref() != Some(row.element_name.as_str()) {
                continue;
            }
            // InstanceID is `Microsoft:{vm-guid}\{nic-guid}\C`; host vNICs
            // have the switch GUID in the first slot and no `\C` suffix.
            if let Some(rest) = entry.instance_id.strip_prefix("Microsoft:") {
                if let Some((vm_part, tail)) = rest.split_once('\\') {
                    if tail.ends_with("\\C") {
                        if let Ok(vm_id) = vm_part.parse::<uuid::Uuid>() {
                            let vm_id = VmId::new(vm_id);
                            if !connected_vm_ids.contains(&vm_id) {
                                connected_vm_ids.push(vm_id);
                            }
                        }
                    }
                }
            }
        }
        // Checkpoint configurations also own NICs (EPASD), but they are not
        // connected VMs. Keep only GUIDs that name a real VM.
        let vm_rows: Vec<VmNameRow> =
            self.run_query("SELECT Name, ElementName FROM Msvm_ComputerSystem")?;
        let known_vms: std::collections::HashSet<uuid::Uuid> = vm_rows
            .iter()
            .filter_map(|row| row.name.parse::<uuid::Uuid>().ok())
            .collect();
        connected_vm_ids.retain(|id| known_vms.contains(id.as_uuid()));
        let summary = SwitchSummary {
            id,
            name: row.element_name,
            kind: switch_kind(None),
            uplink: None,
        };
        Ok(SwitchDetail {
            summary,
            management_os_shared: false,
            vlans: VlanSettings {
                mode: VlanMode::Disabled,
                access_id: None,
                native_id: None,
                allowed_ids: Vec::new(),
            },
            connected_vm_ids,
        })
    }

    fn list_vhds(&mut self, vm: Option<VmId>) -> Result<Vec<AttachedVhd>, HyperVError> {
        if let Some(vm) = vm {
            if self.summary_row_for(vm)?.is_none() {
                return Err(HyperVError::NotFound {
                    kind: TargetKind::Vm,
                    specified: vm.to_string(),
                    raw: None,
                });
            }
        }
        // SDRD holds the VHD path in HostResource; DiskDrive does not. The
        // class also carries the VM's CD/DVD and floppy attachments, so keep
        // only the hard-disk rows (spec 2.5: `vhd list` lists VHDs only).
        let rows: Vec<SdrdRow> = self.run_query(
            "SELECT InstanceID, ResourceSubType, HostResource \
             FROM Msvm_StorageAllocationSettingData",
        )?;
        let rows: Vec<SdrdRow> = rows
            .into_iter()
            .filter(|row| row.resource_sub_type.as_deref() == Some(HARD_DISK_SUBTYPE))
            .collect();
        // Collect the controller GUIDs referenced by the SDRDs so we can look
        // up each controller's ResourceType once (IDE=5, SCSI=6, SATA=7).
        let mut controller_keys: Vec<String> = Vec::new();
        for row in &rows {
            if row.host_resource.as_ref().is_some_and(|h| !h.is_empty()) {
                if let Some(key) = controller_key_prefix(&row.instance_id) {
                    if !controller_keys.contains(&key) {
                        controller_keys.push(key);
                    }
                }
            }
        }
        // One query for all controller RASDs; build a map from the InstanceID
        // prefix to the ResourceType so we can classify each attachment.
        let controller_rows: Vec<ControllerRasdRow> = self
            .run_query("SELECT InstanceID, ResourceType FROM Msvm_ResourceAllocationSettingData")?;
        let mut controller_map: std::collections::HashMap<String, Option<u16>> =
            std::collections::HashMap::new();
        for key in &controller_keys {
            for crow in &controller_rows {
                if crow.instance_id.starts_with(key) && crow.instance_id.matches('\\').count() == 2
                {
                    // `Microsoft:{vm-guid}\{controller-guid}\{number}` has
                    // exactly two backslashes.
                    controller_map.insert(key.clone(), crow.resource_type);
                    break;
                }
            }
        }
        // One bulk query for the names the `VM` column of `vhd list` shows
        // (design 3.8.3: the count is constant, never one per VM).
        let vm_name_rows: Vec<VmNameRow> =
            self.run_query("SELECT Name, ElementName FROM Msvm_ComputerSystem")?;
        let mut vm_names: std::collections::HashMap<uuid::Uuid, String> =
            std::collections::HashMap::new();
        for row in vm_name_rows {
            if let Ok(guid) = row.name.parse::<uuid::Uuid>() {
                vm_names.insert(guid, row.element_name);
            }
        }
        let mut attached = Vec::new();
        for row in rows {
            let path = row
                .host_resource
                .as_ref()
                .and_then(|paths| paths.first().cloned())
                .unwrap_or_default();
            if path.is_empty() {
                continue;
            }
            let Some((vm_guid, controller_number, lun)) = parse_sdrd_instance_id(&row.instance_id)
            else {
                continue;
            };
            if let Some(filter) = vm {
                if VmId::new(vm_guid) != filter {
                    continue;
                }
            }
            // SDRDs also hang off checkpoint configurations, whose first GUID
            // is the checkpoint's `ConfigurationID`, not a VM. Only the
            // configurations present in `Msvm_ComputerSystem` are the VM's own
            // disks (spec 2.5: the disks attached to a VM).
            let Some(vm_name) = vm_names.get(&vm_guid).cloned() else {
                continue;
            };
            // Classify the controller via the RASD map.
            let kind = controller_key_prefix(&row.instance_id)
                .and_then(|key| controller_map.get(&key))
                .map_or(ControllerKind::Unknown(0), |rt| {
                    controller_kind_from_rasd(*rt)
                });
            let info = self.fetch_vhd_info(&path);
            attached.push(AttachedVhd {
                attachment: VhdAttachment {
                    vm_id: VmId::new(vm_guid),
                    vm_name,
                    controller: Controller {
                        kind,
                        number: controller_number,
                        lun,
                    },
                    path: PathBuf::from(&path),
                },
                info,
            });
        }
        Ok(attached)
    }

    fn get_vhd(&mut self, path: &str) -> Result<VhdInfo, HyperVError> {
        self.fetch_vhd_info(path)
            .ok_or_else(|| HyperVError::NotFound {
                kind: TargetKind::Vhd,
                specified: path.to_string(),
                raw: None,
            })
    }

    fn get_thumbnail(
        &mut self,
        id: VmId,
        width: u16,
        height: u16,
    ) -> Result<Thumbnail, HyperVError> {
        let vm_path = vm_object_path(id);
        let host = host_name();
        let service = management_service_path(&host);
        let con = self.connected()?;
        let out = exec_method_with(
            con,
            &service,
            "Msvm_VirtualSystemManagementService",
            "GetVirtualSystemThumbnailImage",
            &[
                ("TargetSystem", Variant::String(vm_path)),
                ("WidthPixels", Variant::UI2(width)),
                ("HeightPixels", Variant::UI2(height)),
            ],
        )?;
        let return_value = return_value(&out)?;
        if return_value != 0 {
            // 32775 is both `InvalidState` and the size-refused answer (M13-V);
            // either way there is no thumbnail to show.
            return Err(HyperVError::InvalidState {
                target: Some(id),
                state: VmState::Unknown(0),
                operation: "GetVirtualSystemThumbnailImage".to_string(),
                raw: Some(RawError::new(format!(
                    "ReturnValue={return_value} (no thumbnail: the VM has no console, \
                     or the requested size is out of range)"
                ))),
            });
        }
        let data: Vec<u8> = match out.get_property("ImageData") {
            Ok(variant) => Vec::<u8>::try_from(variant).map_err(|error| {
                HyperVError::backend(RawError::new(format!(
                    "ImageData was not a byte array: {error}"
                )))
            })?,
            Err(error) => return Err(map_wmi_error(error)),
        };
        decode_thumbnail(width, height, &data).ok_or_else(|| {
            HyperVError::backend(RawError::new(format!(
                "thumbnail data of {} bytes does not match {width}x{height}",
                data.len()
            )))
        })
    }

    fn request_vm_state(
        &mut self,
        id: VmId,
        request: RequestedState,
    ) -> Result<Pending, HyperVError> {
        let con = self.connected()?;
        let path = vm_object_path(id);
        let state_value = requested_state_value(request);
        let out = exec_method_with(
            con,
            &path,
            "Msvm_ComputerSystem",
            "RequestStateChange",
            &[("RequestedState", Variant::UI2(state_value))],
        )?;
        // Design 3.1/3.7.3: ShutDown answers with a job like every other
        // power request; flattening it into a state wait would swallow the
        // job's failure, and a refused shutdown (the guest is not ready,
        // anomaly 16) has to surface as `JobFailed` instead.
        pending_from_method(&out, "RequestStateChange")
    }

    fn create_checkpoint(&mut self, vm: VmId) -> Result<Pending, HyperVError> {
        let con = self.connected()?;
        let host = host_name();
        let service = snapshot_service_path(&host);
        let vm_path = vm_object_path(vm);
        // SnapshotType=2 is the value M0 measured for a normal checkpoint.
        // SnapshotSettings must stay absent (M0 item 6: empty string fails).
        let out = exec_method_with(
            con,
            &service,
            "Msvm_VirtualSystemSnapshotService",
            "CreateSnapshot",
            &[
                ("AffectedSystem", Variant::String(vm_path)),
                ("SnapshotType", Variant::UI2(2)),
            ],
        )?;
        pending_from_method(&out, "CreateSnapshot")
    }

    fn request_guest_reboot(&mut self, id: VmId) -> Result<Pending, HyperVError> {
        // Msvm_ShutdownComponent.InitiateReboot: no job, ReturnValue=0
        // (design 3.3). The component only exists while the VM is Running.
        let uptime_before = self
            .summary_row_for(id)?
            .and_then(|row| row.up_time)
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis);
        let con = self.connected()?;
        let host = host_name();
        let component = shutdown_component_path(&host, id);
        let out = exec_method_with(
            con,
            &component,
            "Msvm_ShutdownComponent",
            "InitiateReboot",
            &[],
        )?;
        let return_value = return_value(&out)?;
        if return_value != 0 {
            // The provider gives no text here; the code alone decides the
            // hint (design 3.8.2, m0-findings: `0x800710DF` when the guest is
            // not ready).
            return Err(HyperVError::JobFailed {
                code: return_value,
                reason: job_fail_reason(return_value),
                raw: None,
            });
        }
        Ok(Pending::AwaitReboot {
            vm: id,
            uptime_before,
        })
    }

    fn rename_checkpoint(
        &mut self,
        checkpoint: CheckpointId,
        name: &str,
    ) -> Result<Pending, HyperVError> {
        // M11-V V4: `ModifySystemSettings` renames a checkpoint by taking its
        // VSSD, serialized as WMI DTD text, with a new `ElementName`. The call
        // returns synchronously with ReturnValue=0 (no job).
        let host = host_name();
        let service = management_service_path(&host);
        let vssd_path = vssd_object_path(&vssd_instance_id(checkpoint));
        let con = self.connected()?;
        let vssd = con.get_object(vssd_path.as_str()).map_err(map_wmi_error)?;
        vssd.put_property("ElementName", name.to_string())
            .map_err(map_wmi_error)?;
        let settings = vssd_to_dtd_text(&vssd)?;
        let out = exec_method_with(
            con,
            &service,
            "Msvm_VirtualSystemManagementService",
            "ModifySystemSettings",
            &[("SystemSettings", Variant::String(settings))],
        )?;
        let return_value = return_value(&out)?;
        if return_value != 0 {
            return Err(HyperVError::backend(RawError::new(format!(
                "ModifySystemSettings failed with {return_value}"
            ))));
        }
        Ok(Pending::Done)
    }

    fn restore_checkpoint(
        &mut self,
        vm: VmId,
        checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError> {
        // ApplySnapshot returns 32775 while the VM is Running (M0 item 6).
        if let Some(row) = self.summary_row_for(vm)? {
            let state = VmState::from_enabled_state(row.enabled_state);
            if state == VmState::Running {
                return Err(HyperVError::InvalidState {
                    target: Some(vm),
                    state,
                    operation: "restore_checkpoint".to_string(),
                    raw: None,
                });
            }
        } else {
            return Err(HyperVError::NotFound {
                kind: TargetKind::Vm,
                specified: vm.to_string(),
                raw: None,
            });
        }
        let con = self.connected()?;
        let host = host_name();
        let service = snapshot_service_path(&host);
        let snapshot_path = vssd_object_path(&vssd_instance_id(checkpoint));
        let out = exec_method_with(
            con,
            &service,
            "Msvm_VirtualSystemSnapshotService",
            "ApplySnapshot",
            &[("Snapshot", Variant::String(snapshot_path))],
        )?;
        pending_from_method(&out, "ApplySnapshot")
    }

    fn remove_checkpoint(&mut self, checkpoint: CheckpointId) -> Result<Pending, HyperVError> {
        let con = self.connected()?;
        let host = host_name();
        let service = snapshot_service_path(&host);
        let snapshot_path = vssd_object_path(&vssd_instance_id(checkpoint));
        let out = exec_method_with(
            con,
            &service,
            "Msvm_VirtualSystemSnapshotService",
            "DestroySnapshot",
            &[("AffectedSnapshot", Variant::String(snapshot_path))],
        )?;
        pending_from_method(&out, "DestroySnapshot")
    }

    fn job_status(&mut self, job: JobRef) -> Result<JobStatus, HyperVError> {
        let id = job.as_str();
        let wql = format!(
            "SELECT JobState, ErrorCode, PercentComplete, ErrorDescription \
             FROM Msvm_ConcreteJob WHERE InstanceID = '{id}'"
        );
        let rows: Vec<JobRow> = self.run_query(&wql)?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| HyperVError::backend(RawError::new(format!("unknown job '{id}'"))))?;
        // M0 item 6 / design 3.7.6: JobState 7=Completed, 10=Exception.
        // Message text is localized (Japanese on this host) so only the code
        // is carried into the error; the hint comes from the code alone
        // (design 3.8.2).
        match row.job_state {
            7 => Ok(JobStatus::Succeeded),
            10 => {
                let code = row.error_code.unwrap_or(0);
                Ok(JobStatus::Failed {
                    code,
                    message: row.error_description.unwrap_or_default(),
                    reason: job_fail_reason(code),
                })
            }
            // CIM: 3=Starting, 4=Running, 5=Canceling, 6=Completed with
            // warnings handled as Succeeded above only when 7.
            11 | 12 => Ok(JobStatus::Cancelled),
            _ => Ok(JobStatus::Running(row.percent_complete)),
        }
    }

    fn subscribe_changes(&mut self) -> Result<mpsc::UnboundedReceiver<ChangeEvent>, HyperVError> {
        // The actor thread holds the main connection; events need their own
        // connection on their own thread (design 3.4, M0 item 7).
        let (tx, rx) = mpsc::unbounded_channel();
        let result = std::thread::Builder::new()
            .name("hypervctl-wmi-events".to_string())
            .spawn(move || event_loop(tx));
        match result {
            Ok(_handle) => {
                // The loop runs until the receiver is dropped; the handle is
                // detached on purpose so the frontend can exit cleanly.
                Ok(rx)
            }
            Err(error) => Err(HyperVError::backend(RawError::new(format!(
                "could not start the WMI event thread: {error}"
            )))),
        }
    }
}

/// The `ResourceSubType` of a virtual hard disk attachment; the CD/DVD and
/// floppy attachments share the SDRD class but not this value (M11-W2).
const HARD_DISK_SUBTYPE: &str = "Microsoft:Hyper-V:Virtual Hard Disk";

/// Reads the embedded-instance text from a method out-param when the call
/// succeeded. The provider returns `SettingData`/`State` as DTD text, and the
/// property is absent (or a different shape) when it had nothing to report.
fn method_embedded_text(out: &IWbemClassWrapper, property: &str) -> Option<String> {
    if return_value(out).ok()? != 0 {
        return None;
    }
    match out.get_property(property) {
        Ok(Variant::String(text)) if !text.is_empty() => Some(text),
        _ => None,
    }
}

/// Extracts a property value from the DTD text of an embedded WMI instance,
/// e.g. `<PROPERTY NAME="Format" TYPE="uint16"><VALUE>3</VALUE></PROPERTY>`.
/// `None` when the property or its value is absent.
fn embedded_property(text: &str, name: &str) -> Option<String> {
    let open = format!("PROPERTY NAME=\"{name}\"");
    let after_open = text.get(text.find(&open)?..)?;
    let after_value = after_open.get(after_open.find("<VALUE>")? + "<VALUE>".len()..)?;
    let value = after_value.get(..after_value.find("</VALUE>")?)?;
    Some(unescape_xml(value))
}

/// Reverses the entity escapes Hyper-V uses in DTD text. Only the five
/// predefined XML entities appear in practice; `&amp;` is expanded last so it
/// cannot double-decode a literal `&lt;` (M11-W2).
fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Assembles a [`VhdInfo`] from the `Msvm_VirtualHardDiskSettingData` and
/// `Msvm_VirtualHardDiskState` instances (design 3.8.3). The format, virtual
/// size and file size are required; a disk the service could not describe
/// fully yields `None` so the columns stay unknown rather than wrong.
fn vhd_info_from_instances(path: &str, setting: &str, state: Option<&str>) -> Option<VhdInfo> {
    let format = match embedded_property(setting, "Format")?.parse::<u16>() {
        Ok(2) => VhdFormat::Vhd,
        Ok(3) => VhdFormat::Vhdx,
        Ok(other) => VhdFormat::Unknown(other),
        Err(_) => return None,
    };
    let kind = match embedded_property(setting, "Type").and_then(|v| v.parse::<u16>().ok()) {
        Some(2) => VhdKind::Fixed,
        Some(3) => VhdKind::Dynamic,
        Some(4) => VhdKind::Differencing,
        Some(other) => VhdKind::Unknown(other),
        None => VhdKind::Unknown(0),
    };
    let virtual_size_bytes = embedded_property(setting, "MaxInternalSize")?
        .parse::<u64>()
        .ok()?;
    let file_size_bytes = embedded_property(state?, "FileSize")?.parse::<u64>().ok()?;
    let block_size_bytes =
        embedded_property(setting, "BlockSize").and_then(|v| v.parse::<u64>().ok());
    let parent_path = embedded_property(setting, "ParentPath")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let disk_id = embedded_property(setting, "VirtualDiskId").filter(|value| !value.is_empty());
    Some(VhdInfo {
        path: PathBuf::from(path),
        format,
        kind,
        virtual_size_bytes,
        file_size_bytes,
        block_size_bytes,
        parent_path,
        disk_id,
    })
}

/// One WMI event that maps onto a [`ChangeEvent`]. The struct names must equal
/// the WMI class names (M0 item 7).
#[derive(Debug, Deserialize)]
#[serde(rename = "__InstanceModificationEvent")]
struct InstanceModificationEvent {
    #[serde(rename = "TargetInstance")]
    target_instance: Option<ComputerSystemTarget>,
}

/// The `TargetInstance` of a VM state event; only `Name` is needed to know a
/// VM changed (the frontend re-reads the state itself, design 3.4).
#[derive(Debug, Deserialize)]
#[serde(rename = "Msvm_ComputerSystem")]
struct ComputerSystemTarget {
    #[serde(rename = "Name")]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "__InstanceCreationEvent")]
struct InstanceCreationEvent {
    #[serde(rename = "TargetInstance")]
    target_instance: Option<VssdTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "__InstanceDeletionEvent")]
struct InstanceDeletionEvent {
    #[serde(rename = "TargetInstance")]
    target_instance: Option<VssdTarget>,
}

/// The `TargetInstance` of a checkpoint create/delete event.
#[derive(Debug, Deserialize)]
#[serde(rename = "Msvm_VirtualSystemSettingData")]
struct VssdTarget {
    #[serde(rename = "VirtualSystemIdentifier")]
    virtual_system_identifier: Option<String>,
}

/// Runs until the receiver is dropped. Each event type gets its own connection
/// because the notification iterators block (M0 item 7). A failed subscription
/// is logged and the thread exits; `ChangeWatcher` re-subscribes (design 3.4).
fn event_loop(tx: mpsc::UnboundedSender<ChangeEvent>) {
    let mut handles = Vec::new();
    for kind in [
        EventKind::VmState,
        EventKind::CheckpointCreated,
        EventKind::CheckpointDeleted,
    ] {
        let tx = tx.clone();
        match std::thread::Builder::new()
            .name(format!("hypervctl-wmi-{kind}"))
            .spawn(move || event_thread(kind, tx))
        {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                tracing::warn!("could not start WMI event thread: {error}");
            }
        }
    }
    // Drop our clone so the channel closes when every subscriber ends.
    drop(tx);
    for handle in handles {
        let _ = handle.join();
    }
}

#[derive(Debug, Clone, Copy)]
enum EventKind {
    VmState,
    CheckpointCreated,
    CheckpointDeleted,
}

impl EventKind {
    const fn query(self) -> &'static str {
        match self {
            Self::VmState => {
                "SELECT * FROM __InstanceModificationEvent WITHIN 1 \
                 WHERE TargetInstance ISA \"Msvm_ComputerSystem\""
            }
            Self::CheckpointCreated => {
                "SELECT * FROM __InstanceCreationEvent WITHIN 1 \
                 WHERE TargetInstance ISA \"Msvm_VirtualSystemSettingData\""
            }
            Self::CheckpointDeleted => {
                "SELECT * FROM __InstanceDeletionEvent WITHIN 1 \
                 WHERE TargetInstance ISA \"Msvm_VirtualSystemSettingData\""
            }
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VmState => f.write_str("vm-state"),
            Self::CheckpointCreated => f.write_str("checkpoint-created"),
            Self::CheckpointDeleted => f.write_str("checkpoint-deleted"),
        }
    }
}

fn event_thread(kind: EventKind, tx: mpsc::UnboundedSender<ChangeEvent>) {
    let con = match WMIConnection::with_namespace_path(NAMESPACE) {
        Ok(con) => con,
        Err(error) => {
            tracing::warn!("WMI event thread {kind} could not connect: {error}");
            return;
        }
    };
    match kind {
        EventKind::VmState => {
            let iter = match con.raw_notification::<InstanceModificationEvent>(kind.query()) {
                Ok(iter) => iter,
                Err(error) => {
                    tracing::warn!("WMI event thread {kind} could not subscribe: {error}");
                    return;
                }
            };
            for event in iter {
                match event {
                    Ok(event) => {
                        // Any VM modification is a signal; the name only
                        // proves the event was for a real VM.
                        if event
                            .target_instance
                            .as_ref()
                            .and_then(|t| t.name.as_ref())
                            .is_some_and(|n| !n.is_empty())
                            && tx.send(ChangeEvent::Vms).is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!("WMI event thread {kind} stream error: {error}");
                        break;
                    }
                }
            }
        }
        EventKind::CheckpointCreated => {
            let iter = match con.raw_notification::<InstanceCreationEvent>(kind.query()) {
                Ok(iter) => iter,
                Err(error) => {
                    tracing::warn!("WMI event thread {kind} could not subscribe: {error}");
                    return;
                }
            };
            for event in iter {
                match event {
                    Ok(event) => {
                        let vm = event
                            .target_instance
                            .as_ref()
                            .and_then(|t| t.virtual_system_identifier.as_deref())
                            .and_then(|id| id.parse::<uuid::Uuid>().ok())
                            .map(VmId::new);
                        if let Some(vm) = vm {
                            if tx.send(ChangeEvent::Checkpoints(vm)).is_err() {
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!("WMI event thread {kind} stream error: {error}");
                        break;
                    }
                }
            }
        }
        EventKind::CheckpointDeleted => {
            let iter = match con.raw_notification::<InstanceDeletionEvent>(kind.query()) {
                Ok(iter) => iter,
                Err(error) => {
                    tracing::warn!("WMI event thread {kind} could not subscribe: {error}");
                    return;
                }
            };
            for event in iter {
                match event {
                    Ok(event) => {
                        let vm = event
                            .target_instance
                            .as_ref()
                            .and_then(|t| t.virtual_system_identifier.as_deref())
                            .and_then(|id| id.parse::<uuid::Uuid>().ok())
                            .map(VmId::new);
                        if let Some(vm) = vm {
                            if tx.send(ChangeEvent::Checkpoints(vm)).is_err() {
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!("WMI event thread {kind} stream error: {error}");
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A captured `GetVirtualHardDiskSettingData` instance (M11-V V6),
    /// re-indented but otherwise verbatim.
    const DEBIAN_AVHDX_SETTING: &str = r#"<INSTANCE CLASSNAME="Msvm_VirtualHardDiskSettingData"><PROPERTY NAME="BlockSize" TYPE="uint32"><VALUE>2097152</VALUE></PROPERTY><PROPERTY NAME="ElementName" TYPE="string"><VALUE>Debian_65AE889E-3919-4C21-978B-545CDF734911.avhdx</VALUE></PROPERTY><PROPERTY NAME="Format" TYPE="uint16"><VALUE>3</VALUE></PROPERTY><PROPERTY NAME="InstanceID" TYPE="string"><VALUE>493C5843-CD5C-40E8-AED3-03D41B65464D</VALUE></PROPERTY><PROPERTY NAME="LogicalSectorSize" TYPE="uint32"><VALUE>512</VALUE></PROPERTY><PROPERTY NAME="MaxInternalSize" TYPE="uint64"><VALUE>136365211648</VALUE></PROPERTY><PROPERTY NAME="ParentPath" TYPE="string"><VALUE>C:\ProgramData\Microsoft\Windows\Virtual Hard Disks\Debian.vhdx</VALUE></PROPERTY><PROPERTY NAME="Path" TYPE="string"><VALUE>C:\ProgramData\Microsoft\Windows\Virtual Hard Disks\Debian_65AE889E-3919-4C21-978B-545CDF734911.avhdx</VALUE></PROPERTY><PROPERTY NAME="PhysicalSectorSize" TYPE="uint32"><VALUE>4096</VALUE></PROPERTY><PROPERTY NAME="Type" TYPE="uint16"><VALUE>4</VALUE></PROPERTY><PROPERTY NAME="VirtualDiskId" TYPE="string"><VALUE>1B2472B2-FDCF-48AD-BCAD-B6A85B36FBC4</VALUE></PROPERTY></INSTANCE>"#;

    /// The matching `GetVirtualHardDiskState` instance (M11-V V6).
    const DEBIAN_AVHDX_STATE: &str = r#"<INSTANCE CLASSNAME="Msvm_VirtualHardDiskState"><PROPERTY NAME="Alignment" TYPE="uint32"><VALUE>1</VALUE></PROPERTY><PROPERTY NAME="FileSize" TYPE="uint64"><VALUE>126877696</VALUE></PROPERTY><PROPERTY NAME="InUse" TYPE="boolean"><VALUE>TRUE</VALUE></PROPERTY><PROPERTY NAME="MinInternalSize" TYPE="uint64"><VALUE>136365211648</VALUE></PROPERTY><PROPERTY NAME="PhysicalSectorSize" TYPE="uint32"><VALUE>4096</VALUE></PROPERTY></INSTANCE>"#;

    #[test]
    fn embedded_property_reads_dtd_values_and_stops_at_the_name() {
        // `Path` must not be confused with `ParentPath`.
        assert_eq!(
            embedded_property(DEBIAN_AVHDX_SETTING, "Path").as_deref(),
            Some(
                r"C:\ProgramData\Microsoft\Windows\Virtual Hard Disks\Debian_65AE889E-3919-4C21-978B-545CDF734911.avhdx"
            )
        );
        assert_eq!(
            embedded_property(DEBIAN_AVHDX_SETTING, "ParentPath").as_deref(),
            Some(r"C:\ProgramData\Microsoft\Windows\Virtual Hard Disks\Debian.vhdx")
        );
        assert_eq!(
            embedded_property(DEBIAN_AVHDX_SETTING, "Format").as_deref(),
            Some("3")
        );
        // A missing property is `None`, never a panic.
        assert_eq!(embedded_property(DEBIAN_AVHDX_SETTING, "Nope"), None);
    }

    #[test]
    fn embedded_property_decodes_entity_escapes() {
        let text = r#"<PROPERTY NAME="Path" TYPE="string"><VALUE>C:\a&amp;b&lt;c&gt;.vhdx</VALUE></PROPERTY>"#;
        assert_eq!(
            embedded_property(text, "Path").as_deref(),
            Some(r"C:\a&b<c>.vhdx")
        );
    }

    #[test]
    fn vhd_info_maps_the_setting_and_state_fields() {
        let info = vhd_info_from_instances(
            r"C:\disk.avhdx",
            DEBIAN_AVHDX_SETTING,
            Some(DEBIAN_AVHDX_STATE),
        )
        .expect("info");
        assert_eq!(info.path, PathBuf::from(r"C:\disk.avhdx"));
        assert_eq!(info.format, VhdFormat::Vhdx);
        assert_eq!(info.kind, VhdKind::Differencing);
        assert_eq!(info.virtual_size_bytes, 136_365_211_648);
        assert_eq!(info.file_size_bytes, 126_877_696);
        assert_eq!(info.block_size_bytes, Some(2_097_152));
        assert_eq!(
            info.parent_path,
            Some(PathBuf::from(
                r"C:\ProgramData\Microsoft\Windows\Virtual Hard Disks\Debian.vhdx"
            ))
        );
        assert_eq!(
            info.disk_id.as_deref(),
            Some("1B2472B2-FDCF-48AD-BCAD-B6A85B36FBC4")
        );
    }

    #[test]
    fn vhd_info_keeps_unknown_format_and_kind_values() {
        let setting = r#"<INSTANCE CLASSNAME="Msvm_VirtualHardDiskSettingData"><PROPERTY NAME="Format" TYPE="uint16"><VALUE>9</VALUE></PROPERTY><PROPERTY NAME="Type" TYPE="uint16"><VALUE>7</VALUE></PROPERTY><PROPERTY NAME="MaxInternalSize" TYPE="uint64"><VALUE>1024</VALUE></PROPERTY></INSTANCE>"#;
        let state = r#"<INSTANCE CLASSNAME="Msvm_VirtualHardDiskState"><PROPERTY NAME="FileSize" TYPE="uint64"><VALUE>512</VALUE></PROPERTY></INSTANCE>"#;
        let info = vhd_info_from_instances(r"C:\disk.vhdx", setting, Some(state)).expect("info");
        assert_eq!(info.format, VhdFormat::Unknown(9));
        assert_eq!(info.kind, VhdKind::Unknown(7));
        assert_eq!(info.block_size_bytes, None);
        assert_eq!(info.parent_path, None);
        assert_eq!(info.disk_id, None);
    }

    #[test]
    fn vhd_info_is_none_without_the_required_fields() {
        // No format.
        assert!(vhd_info_from_instances(
            "x",
            r#"<INSTANCE CLASSNAME="Msvm_VirtualHardDiskSettingData"><PROPERTY NAME="MaxInternalSize"><VALUE>1</VALUE></PROPERTY></INSTANCE>"#,
            Some(r#"<INSTANCE CLASSNAME="Msvm_VirtualHardDiskState"><PROPERTY NAME="FileSize"><VALUE>1</VALUE></PROPERTY></INSTANCE>"#),
        )
        .is_none());
        // No state instance at all.
        assert!(vhd_info_from_instances("x", DEBIAN_AVHDX_SETTING, None).is_none());
    }

    #[test]
    fn controller_prefix_keeps_the_vm_guid_and_original_case() {
        // InstanceIDs observed on a real host (M11-C).
        let sdrd = "Microsoft:24D18F2F-114E-4654-9D3D-CC03106B4C21\\\
                    83F8638B-8DCA-4152-9EDA-2CA8B33039B4\\0\\0\\L";
        let rasd = "Microsoft:24D18F2F-114E-4654-9D3D-CC03106B4C21\\\
                    83F8638B-8DCA-4152-9EDA-2CA8B33039B4\\0";
        let prefix = controller_key_prefix(sdrd).unwrap_or_else(|| unreachable!("prefix"));
        assert_eq!(
            prefix,
            "Microsoft:24D18F2F-114E-4654-9D3D-CC03106B4C21\\\
             83F8638B-8DCA-4152-9EDA-2CA8B33039B4\\"
        );
        // The key must match the controller RASD, but not a key without the VM
        // GUID (the bug M11-C found).
        assert!(rasd.starts_with(&prefix));
        assert_eq!(rasd.matches('\\').count(), 2);
        assert!(!rasd.starts_with("Microsoft:\\83F8638B"));
        assert_eq!(
            parse_sdrd_instance_id(sdrd),
            Some((
                uuid::Uuid::parse_str("24D18F2F-114E-4654-9D3D-CC03106B4C21")
                    .unwrap_or_else(|_| unreachable!("guid")),
                0,
                0
            ))
        );
    }

    #[test]
    #[ignore = "probe: real machine only"]
    fn wmi_round_trips_of_list_vhds_stay_constant_plus_per_disk() {
        let mut backend = WmiBackend::connect_or_unavailable();
        // Prove the connection opened, then reset the counters.
        backend.list_vms().expect("list_vms");
        backend.query_count = 0;
        backend.method_count = 0;
        let attached = backend.list_vhds(None).expect("list_vhds");
        // Three bulk queries: the attachment walk, the controller lookup and the
        // VM names (design 3.8.3: constant, never one per VM or disk).
        assert_eq!(backend.query_count, 3, "list_vhds must not query per VM");
        // One or two ImageManagementService calls per attached disk (design
        // 3.8.3: the state call is skipped when the setting data failed).
        let disks = attached.len() as u32;
        assert!(
            backend.method_count >= disks && backend.method_count <= 2 * disks,
            "{} method calls for {} disks",
            backend.method_count,
            disks
        );
    }

    fn summary_row(
        name: &str,
        element_name: &str,
        enabled_state: u32,
        memory_usage: Option<u64>,
        up_time: Option<u64>,
    ) -> SummaryRow {
        SummaryRow {
            name: name.to_string(),
            element_name: element_name.to_string(),
            enabled_state,
            memory_usage,
            up_time,
            number_of_processors: None,
            processor_load: None,
        }
    }

    #[test]
    fn summary_maps_the_units_verified_in_m0() {
        let row = summary_row(
            "24d18f2f-114e-4654-9d3d-cc03106b4c21",
            "hypervctl-test-debian",
            2,
            Some(1024),
            Some(48_817),
        );
        let summary = summary_from_row(row).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(summary.state, VmState::Running);
        // MemoryUsage is MB; the domain type holds bytes.
        assert_eq!(summary.memory_assigned_bytes, Some(1024 * 1024 * 1024));
        // UpTime is milliseconds.
        assert_eq!(summary.uptime, Some(Duration::from_millis(48_817)));
        assert_eq!(summary.cpu_usage_percent, None);
    }

    #[test]
    fn processor_load_becomes_the_cpu_percentage() {
        let mut row = summary_row(
            "24d18f2f-114e-4654-9d3d-cc03106b4c21",
            "web-01",
            2,
            Some(1024),
            Some(1000),
        );
        row.processor_load = Some(37);
        let summary = summary_from_row(row).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(summary.cpu_usage_percent, Some(37.0));

        // A stale non-null load on an Off VM is still not a measurement.
        let mut row = summary_row(
            "24d18f2f-114e-4654-9d3d-cc03106b4c21",
            "web-02",
            3,
            Some(512),
            None,
        );
        row.processor_load = Some(12);
        let summary = summary_from_row(row).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(summary.cpu_usage_percent, None);
    }

    #[test]
    fn an_off_vm_has_no_uptime_and_no_cpu() {
        let row = summary_row(
            "24d18f2f-114e-4654-9d3d-cc03106b4c21",
            "web-02",
            3,
            Some(512),
            Some(0),
        );
        let summary = summary_from_row(row).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(summary.state, VmState::Off);
        assert_eq!(summary.uptime, None);
        assert_eq!(summary.cpu_usage_percent, None);
        // Assigned memory stays visible even when Off.
        assert_eq!(summary.memory_assigned_bytes, Some(512 * 1024 * 1024));
    }

    #[test]
    fn a_saved_vm_also_reports_no_uptime() {
        let row = summary_row(
            "24d18f2f-114e-4654-9d3d-cc03106b4c21",
            "web-03",
            6,
            Some(512),
            Some(999),
        );
        let summary = summary_from_row(row).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(summary.state, VmState::Saved);
        assert_eq!(summary.uptime, None);
    }

    #[test]
    fn every_enabled_state_maps_through_the_m0_table() {
        assert_eq!(VmState::from_enabled_state(1), VmState::Busy);
        assert_eq!(VmState::from_enabled_state(2), VmState::Running);
        assert_eq!(VmState::from_enabled_state(3), VmState::Off);
        assert_eq!(VmState::from_enabled_state(4), VmState::Stopping);
        assert_eq!(VmState::from_enabled_state(6), VmState::Saved);
        assert_eq!(VmState::from_enabled_state(9), VmState::Paused);
        assert_eq!(VmState::from_enabled_state(10), VmState::Busy);
        assert_eq!(VmState::from_enabled_state(42), VmState::Unknown(42));
    }

    #[test]
    fn a_name_that_is_not_a_guid_is_a_backend_error() {
        let row = summary_row("not-a-guid", "web-01", 2, None, None);
        let error = summary_from_row(row).expect_err("must fail");
        assert_eq!(error.kind(), "backend");
    }

    #[test]
    fn checkpoint_kinds_follow_the_m0_table() {
        assert_eq!(checkpoint_kind(Some(5)), CheckpointKind::Standard);
        assert_eq!(checkpoint_kind(Some(3)), CheckpointKind::Production);
        assert_eq!(checkpoint_kind(Some(4)), CheckpointKind::Production);
        assert_eq!(checkpoint_kind(Some(99)), CheckpointKind::Unknown(99));
        assert_eq!(checkpoint_kind(None), CheckpointKind::Unknown(0));
    }

    #[test]
    fn checkpoint_ids_come_from_the_instance_id() {
        let id = checkpoint_id_from_instance("Microsoft:24d18f2f-114e-4654-9d3d-cc03106b4c21");
        assert!(id.is_some(), "a Microsoft: GUID must parse");
        assert_eq!(checkpoint_id_from_instance("Microsoft:not-a-guid"), None);
        assert_eq!(
            checkpoint_id_from_instance("Other:24d18f2f-114e-4654-9d3d-cc03106b4c21"),
            None
        );
    }

    #[test]
    fn dmtf_timestamps_parse_to_utc() {
        // 2026-10-08 12:34:56 UTC.
        let time = parse_dmtf_time("20261008123456.000000-000")
            .unwrap_or_else(|| panic!("the sample must parse"));
        let expected = days_from_civil(2026, 10, 8) * 86_400 + 12 * 3_600 + 34 * 60 + 56;
        assert_eq!(
            time,
            std::time::UNIX_EPOCH + Duration::from_secs(expected as u64)
        );
        assert_eq!(parse_dmtf_time("garbage"), None);
        assert_eq!(parse_dmtf_time("20261301123456.000000-000"), None);
        assert_eq!(parse_dmtf_time("20260230123456.000000-000"), None);
    }

    #[test]
    fn generation_comes_from_the_vssd_subtype() {
        assert_eq!(
            generation_from_subtype(Some("Microsoft:Hyper-V:SubType:1")),
            Some(1)
        );
        assert_eq!(
            generation_from_subtype(Some("Microsoft:Hyper-V:SubType:2")),
            Some(2)
        );
        assert_eq!(
            generation_from_subtype(Some("Microsoft:Hyper-V:SubType:9")),
            None
        );
        assert_eq!(generation_from_subtype(None), None);
    }

    #[test]
    fn active_configs_are_told_apart_from_checkpoints() {
        // Observed on a real host (M11-C); the old equality check used a
        // value that Hyper-V never reports.
        assert!(is_active_config(Some("Microsoft:Hyper-V:System:Realized")));
        assert!(!is_active_config(Some(
            "Microsoft:Hyper-V:Snapshot:Realized"
        )));
        assert!(!is_active_config(Some("Microsoft:Hyper-V:Realized")));
        assert!(!is_active_config(None));
    }

    #[test]
    fn switch_kind_stays_unknown_until_associations_are_walked() {
        assert_eq!(switch_kind(None), SwitchKind::Unknown(0));
        assert_eq!(switch_kind(Some(1)), SwitchKind::Unknown(0));
    }

    #[test]
    fn the_vm_object_path_uses_the_composite_key() {
        let id = VmId::new(uuid::Uuid::nil());
        let path = vm_object_path(id);
        assert!(path.contains(r#"CreationClassName="Msvm_ComputerSystem""#));
        assert!(path.contains(r#"Name="00000000-0000-0000-0000-000000000000""#));
    }

    #[test]
    fn requested_state_values_follow_the_m0_table() {
        assert_eq!(requested_state_value(RequestedState::Start), 2);
        assert_eq!(requested_state_value(RequestedState::Resume), 2);
        assert_eq!(requested_state_value(RequestedState::TurnOff), 3);
        assert_eq!(requested_state_value(RequestedState::ShutDown), 4);
        assert_eq!(requested_state_value(RequestedState::Save), 6);
        assert_eq!(requested_state_value(RequestedState::Pause), 9);
        assert_eq!(requested_state_value(RequestedState::Reset), 11);
    }

    #[test]
    fn job_fail_reasons_classify_both_observed_code_shapes() {
        // Design 3.8.2: the HRESULT-style codes of design 3.7.9.
        assert_eq!(job_fail_reason(0x8007_10DF), JobFailReason::GuestNotReady);
        assert_eq!(job_fail_reason(0x8007_000E), JobFailReason::HostOutOfMemory);
        // m0-findings: the same failures as the raw `ErrorCode` values the
        // job rows showed on real hardware (32768 / 32778 / 32770).
        assert_eq!(job_fail_reason(32768), JobFailReason::GuestNotReady);
        assert_eq!(job_fail_reason(32778), JobFailReason::HostOutOfMemory);
        // Unsupported operations carry no hint of their own (spec 2.12).
        assert_eq!(job_fail_reason(0x8007_0032), JobFailReason::Other);
        assert_eq!(job_fail_reason(32770), JobFailReason::Other);
        // Anything else stays unclassified; only `-v` explains it.
        assert_eq!(job_fail_reason(0), JobFailReason::Other);
        assert_eq!(job_fail_reason(0x8004_1003), JobFailReason::Other);
    }

    #[test]
    fn checkpoint_rename_needs_a_live_connection() {
        // The rename goes straight to `Msvm_VirtualSystemManagementService`
        // (M11-V V4); without a connection it reports the remembered failure
        // rather than a misleading "not implemented" (implemented in M11-W2).
        let mut backend = WmiBackend::connect_or_unavailable();
        if let State::Unavailable(_) = backend.state {
            let error = backend
                .rename_checkpoint(CheckpointId::new(uuid::Uuid::nil()), "wanted")
                .expect_err("no connection");
            assert_eq!(error.kind(), "hyper_v_unavailable");
        }
    }

    #[test]
    fn the_snapshot_service_path_has_all_four_keys() {
        let path = snapshot_service_path("WIN11");
        assert!(path.contains(r#"CreationClassName="Msvm_VirtualSystemSnapshotService""#));
        assert!(path.contains(r#"Name="vssnapsvc""#));
        assert!(path.contains(r#"SystemCreationClassName="Msvm_ComputerSystem""#));
        assert!(path.contains(r#"SystemName="WIN11""#));
    }

    #[test]
    fn the_management_service_path_is_the_vmms_singleton() {
        let path = management_service_path("WIN11");
        assert!(path.contains(r#"CreationClassName="Msvm_VirtualSystemManagementService""#));
        assert!(path.contains(r#"Name="vmms""#));
        assert!(path.contains(r#"SystemCreationClassName="Msvm_ComputerSystem""#));
        assert!(path.contains(r#"SystemName="WIN11""#));
    }

    #[test]
    fn thumbnail_data_drops_the_length_prefix_and_decodes_rgb565() {
        // Two by one: a big-endian length prefix (= 8, the whole buffer) then
        // one red and one green RGB565 pixel (M13-V).
        let data = [0x00, 0x00, 0x00, 0x08, 0x00, 0xF8, 0xE0, 0x07];
        let image = match decode_thumbnail(2, 1, &data) {
            Some(image) => image,
            None => panic!("a well-formed buffer must decode"),
        };
        assert_eq!(
            image.rgba,
            vec![0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF]
        );
        // A body shorter than the size claims is refused, not padded.
        assert!(decode_thumbnail(2, 1, &data[..6]).is_none());
        assert!(decode_thumbnail(2, 1, &[]).is_none());
    }

    #[test]
    fn the_vssd_path_uses_only_the_instance_id_key() {
        // `InstanceID` is the sole key; a redundant `CreationClassName` makes
        // the path invalid (WBEM_E_INVALID_OBJECT_PATH), so it must not appear.
        let path = vssd_object_path("Microsoft:4D679572-6FAC-464D-8CA4-B960A1AC66CA");
        assert_eq!(
            path,
            r#"\\.\root\virtualization\v2:Msvm_VirtualSystemSettingData.InstanceID="Microsoft:4D679572-6FAC-464D-8CA4-B960A1AC66CA""#
        );
        assert!(!path.contains("CreationClassName"));
    }

    #[test]
    fn instance_ids_are_extracted_from_object_paths() {
        let path = "\\\\.\\root\\virtualization\\v2:Msvm_ConcreteJob.CreationClassName=\"Msvm_ConcreteJob\",InstanceID=\"Microsoft:abc-123\"";
        assert_eq!(
            instance_id_from_path(path).as_deref(),
            Some("Microsoft:abc-123")
        );
        assert_eq!(instance_id_from_path("no-colon"), None);
    }

    #[test]
    fn vssd_instance_ids_round_trip_through_checkpoint_ids() {
        let id = CheckpointId::new(uuid::Uuid::nil());
        let instance = vssd_instance_id(id);
        assert_eq!(instance, "Microsoft:00000000-0000-0000-0000-000000000000");
        let parsed = checkpoint_id_from_instance(&instance);
        assert_eq!(parsed, Some(id));
    }

    #[test]
    fn a_failed_connection_is_returned_from_every_call() {
        // Without Hyper-V the connect fails and every read reports the same
        // classified error instead of panicking.
        let mut backend = WmiBackend::connect_or_unavailable();
        if matches!(backend.state, State::Unavailable(_)) {
            let error = backend.list_vms().expect_err("must fail");
            assert_eq!(error.kind(), "hyper_v_unavailable");
            let error = backend
                .request_vm_state(VmId::new(uuid::Uuid::nil()), RequestedState::Start)
                .expect_err("must fail");
            assert_eq!(error.kind(), "hyper_v_unavailable");
        }
    }

    #[test]
    fn the_image_management_service_singleton_is_named_vhdsvc() {
        let path = image_management_service_path("HOST");
        assert!(path.contains(r#"CreationClassName="Msvm_ImageManagementService""#));
        assert!(path.contains(r#"Name="vhdsvc""#));
        assert!(path.contains(r#"SystemName="HOST""#));
    }
}
