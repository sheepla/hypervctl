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
    HyperV, HyperVError, JobRef, JobStatus, MemoryConfig, Pending, RawError, RequestedState,
    SwitchDetail, SwitchId, SwitchKind, SwitchSummary, TargetKind, VhdAttachment, VhdFormat,
    VhdInfo, VhdKind, VlanMode, VlanSettings, VmDetail, VmId, VmState, VmSummary,
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

/// A row of `Msvm_DiskDrive` (one attached virtual disk).
#[derive(Debug, Deserialize)]
struct DiskDriveRow {
    #[serde(rename = "DeviceNumber")]
    device_number: Option<u32>,
    #[serde(rename = "Connection")]
    connection: Option<Vec<String>>,
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

    /// The realized (non-snapshot) VSSD of a VM, if Hyper-V reported one.
    fn realized_vssd(rows: &[VssdRow]) -> Option<&VssdRow> {
        rows.iter().find(|row| {
            row.virtual_system_type
                .as_deref()
                .is_some_and(|t| t == "Microsoft:Hyper-V:Realized")
        })
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
        // Disks, NICs and integration services need association walks that M7
        // adds; the defaults keep the type honest (0 / empty).
        let memory = MemoryConfig {
            startup_bytes: summary.memory_assigned_bytes.unwrap_or(0),
            dynamic_enabled: false,
            min_bytes: None,
            max_bytes: None,
        };
        Ok(VmDetail {
            summary,
            processor_count: u32::from(processor_count),
            memory,
            disks: Vec::new(),
            nics: Vec::new(),
            checkpoint_count,
            config_path,
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
            .run_query("SELECT Name, ElementName FROM Msvm_VirtualSwitch")
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
        let wql = format!("SELECT Name, ElementName FROM Msvm_VirtualSwitch WHERE Name = '{id}'");
        let mut rows: Vec<SwitchRow> = self.run_query(&wql)?;
        let row = rows.pop().ok_or_else(|| HyperVError::NotFound {
            kind: TargetKind::Switch,
            specified: id.to_string(),
            raw: None,
        })?;
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
            connected_vm_ids: Vec::new(),
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
        let rows: Vec<DiskDriveRow> =
            self.run_query("SELECT DeviceNumber, Connection FROM Msvm_DiskDrive")?;
        let mut attached = Vec::new();
        for row in rows {
            let path = row
                .connection
                .as_ref()
                .and_then(|paths| paths.first().cloned())
                .unwrap_or_default();
            if path.is_empty() {
                continue;
            }
            // The owning VM is not on the DiskDrive row; the controller walk
            // lands with M7. Attachments are still listed so `vhd list` works
            // (design 3.1: file info may be incomplete).
            let info = vhd_info_from_path(Path::new(&path));
            attached.push(AttachedVhd {
                attachment: VhdAttachment {
                    vm_id: VmId::new(uuid::Uuid::nil()),
                    vm_name: String::new(),
                    controller: Controller {
                        kind: ControllerKind::Unknown(0),
                        number: row.device_number.unwrap_or(0) as u8,
                        lun: 0,
                    },
                    path: PathBuf::from(&path),
                },
                info,
            });
        }
        let _ = vm;
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

    fn create_checkpoint(&mut self, vm: VmId, _name: Option<&str>) -> Result<Pending, HyperVError> {
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

/// Reads the file size from the filesystem. A missing or unreadable file
/// yields `None` (design 3.1: information may be incomplete, the attachment
/// itself still exists). The virtual size lives in the VHD footer / VHDX
/// header and is filled in when M7 adds format parsing.
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
    Some(VhdInfo {
        path: path.to_path_buf(),
        format,
        kind: VhdKind::Unknown(0),
        virtual_size_bytes: file_size_bytes,
        file_size_bytes,
        block_size_bytes: None,
        parent_path: None,
        disk_id: None,
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
        assert_eq!(VmState::from_enabled_state(2), VmState::Running);
        assert_eq!(VmState::from_enabled_state(3), VmState::Off);
        assert_eq!(VmState::from_enabled_state(4), VmState::Stopping);
        assert_eq!(VmState::from_enabled_state(6), VmState::Saved);
        assert_eq!(VmState::from_enabled_state(9), VmState::Paused);
        assert_eq!(VmState::from_enabled_state(10), VmState::Starting);
        assert_eq!(VmState::from_enabled_state(1), VmState::Unknown(1));
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
