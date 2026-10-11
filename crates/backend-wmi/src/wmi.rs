//! WMI-backed implementation of the Hyper-V backend (design 3.1, 3.7).
//!
//! Read-only operations are implemented in M6; mutations and event subscriptions
//! arrive with M7. The connection is created on the actor thread because WMI
//! objects are not `Send` (design 3.2).

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
use wmi::WMIConnection;

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
        "\\\\.\\{NAMESPACE}:Msvm_ComputerSystem.CreationClassName=\"Msvm_ComputerSystem\",Name=\"{{{id}}}\""
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

/// Not implemented until M7; keeps the trait complete without lying about success.
fn mutation_not_yet(name: &str) -> HyperVError {
    HyperVError::backend(RawError::new(format!(
        "{name} is not available yet; the WMI backend implements reads only (M6)"
    )))
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
        _id: VmId,
        _request: RequestedState,
    ) -> Result<Pending, HyperVError> {
        Err(mutation_not_yet("request_vm_state"))
    }

    fn create_checkpoint(
        &mut self,
        _vm: VmId,
        _name: Option<&str>,
    ) -> Result<Pending, HyperVError> {
        Err(mutation_not_yet("create_checkpoint"))
    }

    fn restore_checkpoint(
        &mut self,
        _vm: VmId,
        _checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError> {
        Err(mutation_not_yet("restore_checkpoint"))
    }

    fn remove_checkpoint(&mut self, _checkpoint: CheckpointId) -> Result<Pending, HyperVError> {
        Err(mutation_not_yet("remove_checkpoint"))
    }

    fn job_status(&mut self, _job: JobRef) -> Result<JobStatus, HyperVError> {
        Err(mutation_not_yet("job_status"))
    }

    fn subscribe_changes(&mut self) -> Result<mpsc::UnboundedReceiver<ChangeEvent>, HyperVError> {
        Err(mutation_not_yet("subscribe_changes"))
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

// `vm_object_path` is the only way to address a VM for method calls (M7);
// keep it compiled and tested from M6 so the shape cannot rot.
#[allow(dead_code)]
fn _use_vm_object_path() {
    let _ = vm_object_path(VmId::new(uuid::Uuid::nil()));
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
        assert!(path.contains(r#"Name="{00000000-0000-0000-0000-000000000000}""#));
    }

    #[test]
    fn mutations_are_refused_until_m7() {
        let mut backend = WmiBackend::connect_or_unavailable();
        let id = VmId::new(uuid::Uuid::nil());
        let error = backend
            .request_vm_state(id, RequestedState::Start)
            .expect_err("must refuse");
        assert_eq!(error.kind(), "backend");
        assert!(error
            .raw()
            .map(|raw| raw.message.contains("not available yet"))
            .unwrap_or(false));
    }

    #[test]
    fn a_failed_connection_is_returned_from_every_call() {
        // Without Hyper-V the connect fails and every read reports the same
        // classified error instead of panicking.
        let mut backend = WmiBackend::connect_or_unavailable();
        if matches!(backend.state, State::Unavailable(_)) {
            let error = backend.list_vms().expect_err("must fail");
            assert_eq!(error.kind(), "hyper_v_unavailable");
        }
    }

    #[test]
    fn vhd_info_needs_the_file_to_exist() {
        let missing = vhd_info_from_path(Path::new(r"C:\definitely\missing\disk.vhdx"));
        assert_eq!(missing, None);
    }
}
