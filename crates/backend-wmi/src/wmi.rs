//! WMI-backed implementation of the Hyper-V backend (design 3.1, 3.7).
//!
//! Reads and mutations both live here. Mutations follow M0's measured
//! patterns: `RequestStateChange` for power, `Msvm_VirtualSystemSnapshotService`
//! for checkpoints, and out-params as `HashMap<String, Variant>`. The
//! connection is created on the actor thread because WMI objects are not
//! `Send` (design 3.2).

use std::path::{Path, PathBuf};
use std::time::Duration;

use hypervctl_core::{
    AttachedVhd, ChangeEvent, Checkpoint, CheckpointId, CheckpointKind, Controller, ControllerKind,
    HyperV, HyperVError, JobRef, JobStatus, MemoryConfig, NicConfig, Pending, RawError,
    RequestedState, SwitchDetail, SwitchId, SwitchKind, SwitchSummary, TargetKind, VhdAttachment,
    VhdFormat, VhdInfo, VhdKind, VlanMode, VlanSettings, VmDetail, VmId, VmState, VmSummary,
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
#[derive(Debug, Deserialize)]
struct SdrdRow {
    #[serde(rename = "InstanceID")]
    instance_id: String,
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
            },
            Err(error) => Self {
                state: State::Unavailable(map_wmi_error(error)),
                #[cfg(test)]
                query_count: 0,
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

    fn summary_rows(&mut self) -> Result<Vec<SummaryRow>, HyperVError> {
        // One query for every VM (design 3.1: N+1 forbidden). `Snapshots` and
        // `__Path` are left out on purpose (M0 items 1–2).
        self.run_query(
            "SELECT Name, ElementName, EnabledState, MemoryUsage, UpTime, NumberOfProcessors \
             FROM Msvm_SummaryInformation",
        )
    }

    fn summary_row_for(&mut self, id: VmId) -> Result<Option<SummaryRow>, HyperVError> {
        let wql = format!(
            "SELECT Name, ElementName, EnabledState, MemoryUsage, UpTime, NumberOfProcessors \
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
        rows.iter().find(|row| {
            row.virtual_system_type
                .as_deref()
                .is_some_and(|t| t == "Microsoft:Hyper-V:Realized")
        })
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
    // Off VMs report no CPU usage and no uptime (spec 2.10). CPU usage is not
    // on SummaryInformation at all, so it stays None for every state.
    let cpu_usage_percent = None;
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

/// Target `EnabledState` once the request finishes; used for
/// `Pending::AwaitState` and for idempotency checks upstream.
fn target_enabled_state(request: RequestedState) -> VmState {
    match request {
        RequestedState::Start | RequestedState::Resume | RequestedState::Reset => VmState::Running,
        RequestedState::ShutDown | RequestedState::TurnOff => VmState::Off,
        RequestedState::Save => VmState::Saved,
        RequestedState::Pause => VmState::Paused,
    }
}

/// Builds the WMI object path for the snapshot service (M0 item 6: four-key
/// composite; `SystemName` is the host name).
fn snapshot_service_path(host: &str) -> String {
    format!(
        "\\\\.\\{NAMESPACE}:Msvm_VirtualSystemSnapshotService.CreationClassName=\"Msvm_VirtualSystemSnapshotService\",Name=\"vssnapsvc\",SystemCreationClassName=\"Msvm_ComputerSystem\",SystemName=\"{host}\""
    )
}

/// Builds the WMI object path for a VM's shutdown component (design 3.3,
/// `Msvm_ShutdownComponent`). Four-key composite, like the snapshot service.
fn shutdown_component_path(host: &str, vm: VmId) -> String {
    format!(
        "\\\\.\\{NAMESPACE}:Msvm_ShutdownComponent.CreationClassName=\"Msvm_ShutdownComponent\",Name=\"{vm}\",SystemCreationClassName=\"Msvm_ComputerSystem\",SystemName=\"{host}\""
    )
}

/// Builds the WMI object path for a VSSD instance (M0 item 6: `InstanceID` is
/// the sole key of `Msvm_VirtualSystemSettingData`).
fn vssd_object_path(instance_id: &str) -> String {
    format!(
        "\\\\.\\{NAMESPACE}:Msvm_VirtualSystemSettingData.CreationClassName=\"Msvm_VirtualSystemSettingData\",InstanceID=\"{instance_id}\""
    )
}

/// `InstanceID` of the VSSD behind a checkpoint id (`Microsoft:<guid>`).
fn vssd_instance_id(checkpoint: CheckpointId) -> String {
    format!("Microsoft:{checkpoint}")
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
        rows.into_iter().map(summary_from_row).collect()
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
            // Integration services need a class that is not registered on
            // all hosts; the default keeps the type honest (empty).
            integration_services: Vec::new(),
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
        // SDRD holds the VHD path in HostResource; DiskDrive does not.
        let rows: Vec<SdrdRow> = self
            .run_query("SELECT InstanceID, HostResource FROM Msvm_StorageAllocationSettingData")?;
        // Collect the controller GUIDs referenced by the SDRDs so we can look
        // up each controller's ResourceType once (IDE=5, SCSI=6, SATA=7).
        let mut controller_keys: Vec<String> = Vec::new();
        for row in &rows {
            if row.host_resource.as_ref().is_some_and(|h| !h.is_empty()) {
                if let Some(rest) = row.instance_id.strip_prefix("Microsoft:") {
                    if let Some((vm_guid, _num, _lun)) = parse_sdrd_instance_id(&row.instance_id) {
                        let _ = vm_guid;
                        if let Some(controller_guid) = rest.split('\\').nth(1) {
                            let key = format!("Microsoft:\\{controller_guid}\\");
                            if !controller_keys.contains(&key) {
                                controller_keys.push(key);
                            }
                        }
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
                    // `Microsoft:\{guid}\{number}` has exactly two backslashes.
                    controller_map.insert(key.clone(), crow.resource_type);
                    break;
                }
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
            // Classify the controller via the RASD map.
            let kind = row
                .instance_id
                .strip_prefix("Microsoft:")
                .and_then(|rest| rest.split('\\').nth(1))
                .and_then(|cg| controller_map.get(&format!("Microsoft:\\{cg}\\")))
                .map_or(ControllerKind::Unknown(0), |rt| {
                    controller_kind_from_rasd(*rt)
                });
            let info = vhd_info_from_path(Path::new(&path));
            attached.push(AttachedVhd {
                attachment: VhdAttachment {
                    vm_id: VmId::new(vm_guid),
                    vm_name: String::new(),
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
        vhd_info_from_path(Path::new(path)).ok_or_else(|| HyperVError::NotFound {
            kind: TargetKind::Vhd,
            specified: path.to_string(),
            raw: None,
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
        // Guest shutdown has no job when Hyper-V accepts it; the state is the
        // progress (design 3.1). Other requests return 4096+Job or 0.
        let pending = pending_from_method(&out, "RequestStateChange")?;
        if request == RequestedState::ShutDown && matches!(pending, Pending::Job(_)) {
            return Ok(Pending::AwaitState(id, target_enabled_state(request)));
        }
        Ok(pending)
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
            // 0x800710DF is the guest-not-ready code seen on real hardware
            // (M0 item 5, V10).
            return Err(HyperVError::JobFailed {
                code: return_value,
                message: format!("InitiateReboot returned {return_value}"),
                reason: hypervctl_core::JobFailReason::GuestNotReady,
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
        _name: &str,
    ) -> Result<Pending, HyperVError> {
        // V4 (design 3.8.8) is unverified: which WMI method renames a
        // checkpoint has not been confirmed on real hardware. Until M11-V
        // verifies it, report a clear error rather than guessing (design
        // 3.8: "推測で実装しない"). The operation layer already reports the
        // created checkpoint's id so the user can clean up (design 3.3).
        let _ = checkpoint;
        Err(HyperVError::backend(RawError::new(
            "checkpoint rename is not verified on real hardware yet (M11-V); \
             the checkpoint was created with the provider's default name",
        )))
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
        // is carried into the error.
        match row.job_state {
            7 => Ok(JobStatus::Succeeded),
            10 => Ok(JobStatus::Failed {
                code: row.error_code.unwrap_or(0),
                message: row.error_description.unwrap_or_default(),
            }),
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

/// Reads the file size and virtual size from the VHD/VHDX header. A missing
/// or unreadable file yields `None` (design 3.1: information may be
/// incomplete, the attachment itself still exists).
fn vhd_info_from_path(path: &Path) -> Option<VhdInfo> {
    let metadata = std::fs::metadata(path).ok()?;
    let file_size_bytes = metadata.len();
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let format = match extension.as_deref() {
        Some("vhd") => VhdFormat::Vhd,
        Some("vhdx") => VhdFormat::Vhdx,
        _ => return None,
    };
    let virtual_size_bytes = match format {
        VhdFormat::Vhd => read_vhd_virtual_size(path).unwrap_or(file_size_bytes),
        VhdFormat::Vhdx => read_vhdx_virtual_size(path).unwrap_or(file_size_bytes),
        VhdFormat::Unknown(_) => file_size_bytes,
    };
    let (kind, parent_path) = match format {
        VhdFormat::Vhd => read_vhd_footer(path)
            .map(|f| (f.kind, f.parent_path))
            .unwrap_or((VhdKind::Unknown(0), None)),
        _ => (VhdKind::Unknown(0), None),
    };
    Some(VhdInfo {
        path: path.to_path_buf(),
        format,
        kind,
        virtual_size_bytes,
        file_size_bytes,
        block_size_bytes: None,
        parent_path,
        disk_id: None,
    })
}

/// VHD footer fields we care about (512 bytes, big-endian numbers).
struct VhdFooter {
    kind: VhdKind,
    virtual_size: u64,
    parent_path: Option<PathBuf>,
}

/// Reads the VHD footer. Fixed disks store it at offset 0; dynamic and
/// differencing disks at `file_size - 512`. Both locations are tried.
fn read_vhd_footer(path: &Path) -> Option<VhdFooter> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len < 512 {
        return None;
    }
    // Try the end first (dynamic/differencing), then the start (fixed).
    let mut buf = [0u8; 512];
    for offset in [file_len.wrapping_sub(512), 0] {
        if file.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }
        if file.read_exact(&mut buf).is_err() {
            continue;
        }
        // Cookie at offset 0 must be "conectix".
        if &buf[0..8] != b"conectix" {
            continue;
        }
        let disk_type = u32::from_be_bytes([buf[60], buf[61], buf[62], buf[63]]);
        let virtual_size = u64::from_be_bytes([
            buf[48], buf[49], buf[50], buf[51], buf[52], buf[53], buf[54], buf[55],
        ]);
        let kind = match disk_type {
            2 => VhdKind::Fixed,
            3 => VhdKind::Dynamic,
            4 => VhdKind::Differencing,
            other => VhdKind::Unknown(other as u16),
        };
        // Parent locator for differencing disks (offset 64, 8 bytes BE).
        let parent_path = if disk_type == 4 {
            let plat_loc_off = u64::from_be_bytes([
                buf[72], buf[73], buf[74], buf[75], buf[76], buf[77], buf[78], buf[79],
            ]);
            if plat_loc_off > 0 && plat_loc_off < file_len {
                let _ = file.seek(SeekFrom::Start(plat_loc_off));
                let mut pl = [0u8; 8];
                if file.read_exact(&mut pl).is_ok() {
                    let data_off = u64::from_be_bytes(pl) + 8; // skip header
                    let data_len = u16::from_be_bytes([buf[64], buf[65]]) as u64;
                    if data_off + data_len <= file_len && data_len > 0 && data_len < 2048 {
                        let _ = file.seek(SeekFrom::Start(data_off));
                        let mut raw = vec![0u8; data_len as usize];
                        if file.read_exact(&mut raw).is_ok() {
                            // UTF-16LE in practice; fall back to lossy.
                            let s: String = raw
                                .chunks(2)
                                .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                                .filter_map(|u| char::from_u32(u32::from(u)))
                                .collect();
                            if !s.is_empty() {
                                Some(PathBuf::from(s))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        return Some(VhdFooter {
            kind,
            virtual_size,
            parent_path,
        });
    }
    None
}

fn read_vhd_virtual_size(path: &Path) -> Option<u64> {
    read_vhd_footer(path).map(|f| f.virtual_size)
}

/// VHDX: "vhdxfile" signature at offset 0; VirtualSize lives in the metadata
/// region (defined by the region table at 0x30000). The VirtualDiskSize item
/// is at metadata offset 0x10008 (8 bytes LE) on standard Hyper-V VHDX files.
fn read_vhdx_virtual_size(path: &Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len < 328 {
        return None;
    }
    // Verify the file signature.
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut sig = [0u8; 8];
    file.read_exact(&mut sig).ok()?;
    if &sig != b"vhdxfile" {
        return None;
    }
    // The region table is at 0x30000 (after the two 64KB headers).
    // It contains the metadata region offset.
    file.seek(SeekFrom::Start(0x30000)).ok()?;
    let mut region_hdr = [0u8; 16];
    file.read_exact(&mut region_hdr).ok()?;
    if &region_hdr[0..4] != b"regi" {
        return None;
    }
    let entry_count =
        u32::from_le_bytes([region_hdr[8], region_hdr[9], region_hdr[10], region_hdr[11]]);
    // The metadata region GUID (file bytes): 06 a2 7c 8b 90 47 9a 4b b8 fe 57 5f 05 0f 88 6e
    const META_GUID: [u8; 16] = [
        0x06, 0xa2, 0x7c, 0x8b, 0x90, 0x47, 0x9a, 0x4b, 0xb8, 0xfe, 0x57, 0x5f, 0x05, 0x0f, 0x88,
        0x6e,
    ];
    let mut meta_offset = None;
    for e in 0..entry_count as u64 {
        file.seek(SeekFrom::Start(0x30000 + 16 + e * 32)).ok()?;
        let mut entry = [0u8; 32];
        file.read_exact(&mut entry).ok()?;
        if entry[0..16] == META_GUID {
            meta_offset = Some(u64::from_le_bytes([
                entry[16], entry[17], entry[18], entry[19], entry[20], entry[21], entry[22],
                entry[23],
            ]));
            break;
        }
    }
    let meta_base = meta_offset?;
    // The VirtualDiskSize item is at metadata offset 0x10008 (8 bytes LE).
    file.seek(SeekFrom::Start(meta_base + 0x10008)).ok()?;
    let mut buf = [0u8; 8];
    file.read_exact(&mut buf).ok()?;
    let size = u64::from_le_bytes(buf);
    if size == 0 {
        return None;
    }
    Some(size)
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
    fn target_states_match_the_request() {
        assert_eq!(
            target_enabled_state(RequestedState::Start),
            VmState::Running
        );
        assert_eq!(
            target_enabled_state(RequestedState::Resume),
            VmState::Running
        );
        assert_eq!(
            target_enabled_state(RequestedState::Reset),
            VmState::Running
        );
        assert_eq!(target_enabled_state(RequestedState::ShutDown), VmState::Off);
        assert_eq!(target_enabled_state(RequestedState::TurnOff), VmState::Off);
        assert_eq!(target_enabled_state(RequestedState::Save), VmState::Saved);
        assert_eq!(target_enabled_state(RequestedState::Pause), VmState::Paused);
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
    fn the_vssd_path_uses_the_instance_id() {
        let path = vssd_object_path("Microsoft:4D679572-6FAC-464D-8CA4-B960A1AC66CA");
        assert!(path.contains(r#"CreationClassName="Msvm_VirtualSystemSettingData""#));
        assert!(path.contains(r#"InstanceID="Microsoft:4D679572-6FAC-464D-8CA4-B960A1AC66CA""#));
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
    fn vhd_info_needs_the_file_to_exist() {
        let missing = vhd_info_from_path(Path::new(r"C:\definitely\missing\disk.vhdx"));
        assert_eq!(missing, None);
    }
}
