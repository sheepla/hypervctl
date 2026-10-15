//! The view types behind the output contract (design 4.4): one view per
//! command, converted from the domain types and rendered as table, JSON or
//! CSV. Only the views are `Serialize`; the domain types stay plain.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::Serialize;

use hypervctl_core::{
    AttachedVhd, Checkpoint, CheckpointKind, Controller, ControllerKind, IntegrationStatus,
    MemoryConfig, NicConfig, SwitchDetail, SwitchKind, SwitchSummary, VhdFormat, VhdInfo, VhdKind,
    VlanMode, VmDetail, VmState, VmSummary,
};

use super::format;
use super::table::{Cell, Color};
use super::{DetailTable, ListTable, RenderOptions};

/// What an enum contributes to the three formats (design 4.4): the label for
/// `table`, the value for JSON and CSV, and the raw provider value when the
/// backend could not map it.
type Parts = (String, String, Option<u32>);

fn known(label: &str, value: &str) -> Parts {
    (label.to_string(), value.to_string(), None)
}

fn unknown(raw: u32) -> Parts {
    (format!("Unknown({raw})"), "unknown".to_string(), Some(raw))
}

fn state_parts(state: VmState) -> Parts {
    match state {
        VmState::Running => known("Running", "running"),
        VmState::Off => known("Off", "off"),
        VmState::Saved => known("Saved", "saved"),
        VmState::Paused => known("Paused", "paused"),
        VmState::Starting => known("Starting", "starting"),
        VmState::Stopping => known("Stopping", "stopping"),
        VmState::Busy => known("Busy", "busy"),
        VmState::Unknown(raw) => unknown(raw),
    }
}

/// The word a state is shown as, for every frontend (design 4.4 and 5.9).
pub fn state_label(state: VmState) -> String {
    state_parts(state).0
}

fn checkpoint_kind_parts(kind: CheckpointKind) -> Parts {
    match kind {
        CheckpointKind::Standard => known("Standard", "standard"),
        CheckpointKind::Production => known("Production", "production"),
        CheckpointKind::Unknown(raw) => unknown(raw),
    }
}

fn switch_kind_parts(kind: SwitchKind) -> Parts {
    match kind {
        SwitchKind::External => known("External", "external"),
        SwitchKind::Internal => known("Internal", "internal"),
        SwitchKind::Private => known("Private", "private"),
        SwitchKind::Unknown(raw) => unknown(raw),
    }
}

fn vlan_mode_parts(mode: VlanMode) -> Parts {
    match mode {
        VlanMode::Disabled => known("Disabled", "disabled"),
        VlanMode::Access => known("Access", "access"),
        VlanMode::Trunk => known("Trunk", "trunk"),
        VlanMode::Native => known("Native", "native"),
        VlanMode::Unknown(raw) => unknown(raw),
    }
}

fn vhd_format_parts(format: VhdFormat) -> Parts {
    match format {
        VhdFormat::Vhd => known("Vhd", "vhd"),
        VhdFormat::Vhdx => known("Vhdx", "vhdx"),
        VhdFormat::Unknown(raw) => unknown(u32::from(raw)),
    }
}

fn vhd_kind_parts(kind: VhdKind) -> Parts {
    match kind {
        VhdKind::Fixed => known("Fixed", "fixed"),
        VhdKind::Dynamic => known("Dynamic", "dynamic"),
        VhdKind::Differencing => known("Differencing", "differencing"),
        VhdKind::Unknown(raw) => unknown(u32::from(raw)),
    }
}

fn integration_status_parts(status: IntegrationStatus) -> Parts {
    match status {
        IntegrationStatus::Operational => known("Operational", "operational"),
        IntegrationStatus::Error => known("Error", "error"),
        IntegrationStatus::Unknown(raw) => unknown(u32::from(raw)),
    }
}

/// The controller of an attachment: `SCSI` in the table (spec 2.10 shows
/// `SCSI 0:1`), `scsi` in JSON and CSV.
fn controller_parts(controller: Controller) -> Parts {
    match controller.kind {
        ControllerKind::Ide => known("IDE", "ide"),
        ControllerKind::Scsi => known("SCSI", "scsi"),
        ControllerKind::Sata => known("SATA", "sata"),
        ControllerKind::Unknown(raw) => unknown(u32::from(raw)),
    }
}

/// The colour of a state cell: green at rest, yellow while moving, red for a
/// value the backend could not map (design 4.4).
fn state_color(state: &str) -> Option<Color> {
    match state {
        "running" => Some(Color::Green),
        "starting" | "stopping" | "saving" | "pausing" | "resuming" | "resetting" => {
            Some(Color::Yellow)
        }
        "unknown" => Some(Color::Red),
        _ => None,
    }
}

fn state_cell(label: &str, value: &str) -> Cell {
    match state_color(value) {
        Some(color) => Cell::painted(label, color),
        None => Cell::new(label),
    }
}

/// One line of `vm list` (spec 2.10).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VmListRow {
    pub id: String,
    pub name: String,
    pub state: String,
    pub state_raw: Option<u32>,
    pub cpu_percent: Option<f64>,
    pub memory_assigned_bytes: Option<u64>,
    pub uptime_seconds: Option<u64>,
    pub generation: Option<u8>,
    #[serde(skip)]
    pub state_label: String,
}

impl VmListRow {
    pub fn from_list(mut vms: Vec<VmSummary>) -> Vec<Self> {
        vms.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        vms.into_iter().map(Self::from).collect()
    }
}

impl From<VmSummary> for VmListRow {
    fn from(vm: VmSummary) -> Self {
        let (state, value, state_raw) = state_parts(vm.state);
        Self {
            id: vm.id.to_string(),
            name: vm.name,
            state: value,
            state_raw,
            cpu_percent: vm.cpu_usage_percent,
            memory_assigned_bytes: vm.memory_assigned_bytes,
            uptime_seconds: vm.uptime.map(|uptime| uptime.as_secs()),
            generation: vm.generation,
            state_label: state,
        }
    }
}

impl ListTable for Vec<VmListRow> {
    const HEADERS: &'static [&'static str] =
        &["NAME", "ID", "STATE", "CPU", "MEMORY", "UPTIME", "GEN"];

    fn table_rows(&self, opts: &RenderOptions) -> Vec<Vec<Cell>> {
        self.iter()
            .map(|row| {
                vec![
                    Cell::new(row.name.clone()),
                    Cell::new(format::short_id(&row.id, opts.no_trunc)),
                    state_cell(&row.state_label, &row.state),
                    Cell::dash(row.cpu_percent.map(format::percent)),
                    Cell::dash(row.memory_assigned_bytes.map(format::size)),
                    Cell::dash(
                        row.uptime_seconds
                            .map(|seconds| format::uptime(Duration::from_secs(seconds))),
                    ),
                    Cell::dash(row.generation.map(|generation| generation.to_string())),
                ]
            })
            .collect()
    }
}

/// Startup and dynamic memory settings as JSON shows them (design 4.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryView {
    pub startup_bytes: u64,
    pub dynamic_enabled: bool,
    pub min_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
}

impl From<&MemoryConfig> for MemoryView {
    fn from(memory: &MemoryConfig) -> Self {
        Self {
            startup_bytes: memory.startup_bytes,
            dynamic_enabled: memory.dynamic_enabled,
            min_bytes: memory.min_bytes,
            max_bytes: memory.max_bytes,
        }
    }
}

/// One attached disk of `vm get`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskView {
    pub path: String,
    pub controller: String,
    pub controller_number: u8,
    pub controller_lun: u8,
    pub controller_raw: Option<u32>,
    #[serde(skip)]
    pub controller_label: String,
}

/// One network adapter of `vm get`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NicView {
    pub switch_name: Option<String>,
    pub mac_address: Option<String>,
}

impl From<&NicConfig> for NicView {
    fn from(nic: &NicConfig) -> Self {
        Self {
            switch_name: nic.switch_name.clone(),
            mac_address: nic.mac_address.clone(),
        }
    }
}

/// One guest integration service of `vm get`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IntegrationView {
    pub name: String,
    pub enabled: bool,
    pub status: String,
    pub status_raw: Option<u32>,
    #[serde(skip)]
    pub status_label: String,
}

/// Everything `vm get` prints (spec 2.10).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VmDetailView {
    pub id: String,
    pub name: String,
    pub state: String,
    pub state_raw: Option<u32>,
    pub cpu_percent: Option<f64>,
    pub uptime_seconds: Option<u64>,
    pub generation: Option<u8>,
    pub processor_count: u32,
    pub memory: MemoryView,
    pub disks: Vec<DiskView>,
    pub nics: Vec<NicView>,
    pub checkpoint_count: u32,
    pub config_path: String,
    /// `null` when the services could not be listed (design 3.8.5).
    pub integration_services: Option<Vec<IntegrationView>>,
    pub notes: Option<String>,
    #[serde(skip)]
    pub state_label: String,
}

impl VmDetailView {
    pub fn from_detail(detail: VmDetail) -> Self {
        let VmDetail {
            summary,
            processor_count,
            memory,
            disks,
            nics,
            checkpoint_count,
            config_path,
            integration_services,
            notes,
        } = detail;
        let (state, value, state_raw) = state_parts(summary.state);
        Self {
            id: summary.id.to_string(),
            name: summary.name,
            state: value,
            state_raw,
            cpu_percent: summary.cpu_usage_percent,
            uptime_seconds: summary.uptime.map(|uptime| uptime.as_secs()),
            generation: summary.generation,
            processor_count,
            memory: MemoryView::from(&memory),
            disks: disks
                .iter()
                .map(|attachment| {
                    let (label, value, controller_raw) = controller_parts(attachment.controller);
                    DiskView {
                        path: attachment.path.to_string_lossy().into_owned(),
                        controller: value,
                        controller_number: attachment.controller.number,
                        controller_lun: attachment.controller.lun,
                        controller_raw,
                        controller_label: format!(
                            "{label} {}:{}",
                            attachment.controller.number, attachment.controller.lun
                        ),
                    }
                })
                .collect(),
            nics: nics.iter().map(NicView::from).collect(),
            checkpoint_count,
            config_path,
            integration_services: integration_services.map(|services| {
                services
                    .into_iter()
                    .map(|service| {
                        let (label, value, status_raw) = integration_status_parts(service.status);
                        IntegrationView {
                            name: service.name,
                            enabled: service.enabled,
                            status: value,
                            status_raw,
                            status_label: label,
                        }
                    })
                    .collect()
            }),
            notes,
            state_label: state,
        }
    }
}

impl DetailTable for VmDetailView {
    fn detail_rows(&self, opts: &RenderOptions) -> Vec<(String, Cell)> {
        let mut rows = vec![
            ("Name".to_string(), Cell::new(self.name.clone())),
            (
                "Id".to_string(),
                Cell::new(format::short_id(&self.id, opts.no_trunc)),
            ),
            (
                "State".to_string(),
                state_cell(&self.state_label, &self.state),
            ),
            (
                "Generation".to_string(),
                Cell::dash(self.generation.map(|generation| generation.to_string())),
            ),
            (
                "CPU".to_string(),
                Cell::dash(self.cpu_percent.map(format::percent)),
            ),
            (
                "Uptime".to_string(),
                Cell::dash(
                    self.uptime_seconds
                        .map(|seconds| format::uptime(Duration::from_secs(seconds))),
                ),
            ),
            (
                "Processors".to_string(),
                Cell::new(self.processor_count.to_string()),
            ),
            (
                "Startup memory".to_string(),
                Cell::new(format::size(self.memory.startup_bytes)),
            ),
            (
                "Dynamic memory".to_string(),
                Cell::new(if self.memory.dynamic_enabled {
                    "enabled"
                } else {
                    "disabled"
                }),
            ),
            (
                "Minimum memory".to_string(),
                Cell::dash(self.memory.min_bytes.map(format::size)),
            ),
            (
                "Maximum memory".to_string(),
                Cell::dash(self.memory.max_bytes.map(format::size)),
            ),
            (
                "Checkpoint count".to_string(),
                Cell::new(self.checkpoint_count.to_string()),
            ),
            (
                "Configuration file".to_string(),
                Cell::new(self.config_path.clone()),
            ),
        ];
        for (index, disk) in self.disks.iter().enumerate() {
            rows.push((
                format!("Disk {index}"),
                Cell::new(format!("{}  ({})", disk.path, disk.controller_label)),
            ));
        }
        for (index, nic) in self.nics.iter().enumerate() {
            rows.push((format!("Nic {index}"), Cell::new(nic_label(nic))));
        }
        match &self.integration_services {
            // Not readable (e.g. the VM is not running): one `-` row, which an
            // empty service list would not produce (design 3.8.5).
            None => rows.push(("Services".to_string(), Cell::dash(None::<String>))),
            Some(services) => {
                for service in services {
                    let label = format!(
                        "{}, {}",
                        if service.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        service.status_label
                    );
                    rows.push((format!("Service: {}", service.name), Cell::new(label)));
                }
            }
        }
        rows.push(("Notes".to_string(), Cell::dash(self.notes.clone())));
        rows
    }
}

/// How one adapter reads in `vm get`: what it is connected to, its address and
/// its addresses, whatever exists.
fn nic_label(nic: &NicView) -> String {
    let mut parts = vec![nic
        .switch_name
        .clone()
        .unwrap_or_else(|| "not connected".to_string())];
    if let Some(mac) = &nic.mac_address {
        parts.push(mac.clone());
    }
    parts.join(", ")
}

/// One line of `checkpoint list` (spec 2.10).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckpointListRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub kind_raw: Option<u32>,
    pub created_at: Option<String>,
    pub parent_id: Option<String>,
    #[serde(skip)]
    pub kind_label: String,
    #[serde(skip)]
    pub created_local: String,
    /// Depth in the tree, used for the indentation of `table` only.
    #[serde(skip)]
    pub depth: usize,
}

impl CheckpointListRow {
    /// Orders the checkpoints as a tree: parents before children, names in
    /// ascending order, orphans and cycles still shown (design 4.4).
    pub fn from_list(checkpoints: Vec<Checkpoint>) -> Vec<Self> {
        let mut sorted = checkpoints;
        sorted.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));

        let mut parents: HashSet<String> = HashSet::new();
        let mut children: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, checkpoint) in sorted.iter().enumerate() {
            if let Some(parent) = &checkpoint.parent_id {
                parents.insert(parent.to_string());
                children.entry(parent.to_string()).or_default().push(index);
            }
        }

        let mut visited = vec![false; sorted.len()];
        let mut order = Vec::with_capacity(sorted.len());
        for (index, checkpoint) in sorted.iter().enumerate() {
            let orphan = checkpoint
                .parent_id
                .as_ref()
                .is_none_or(|parent| !parents.contains(&parent.to_string()));
            if orphan {
                visit(index, 0, &sorted, &children, &mut visited, &mut order);
            }
        }
        // Leftovers are a cycle nobody reaches from the top, or a parent that
        // sorts after its child: they still belong in the output.
        for index in 0..sorted.len() {
            if !visited[index] {
                visit(index, 0, &sorted, &children, &mut visited, &mut order);
            }
        }

        order
            .into_iter()
            .map(|(index, depth)| Self::from_checkpoint(&sorted[index], depth))
            .collect()
    }

    fn from_checkpoint(checkpoint: &Checkpoint, depth: usize) -> Self {
        let (kind, value, kind_raw) = checkpoint_kind_parts(checkpoint.kind);
        Self {
            id: checkpoint.id.to_string(),
            name: checkpoint.name.clone(),
            kind: value,
            kind_raw,
            created_at: format::iso(checkpoint.created),
            parent_id: checkpoint.parent_id.map(|id| id.to_string()),
            kind_label: kind,
            created_local: format::local_time(checkpoint.created),
            depth,
        }
    }
}

/// Emits `index` and then its children, depth first.
fn visit(
    index: usize,
    depth: usize,
    sorted: &[Checkpoint],
    children: &HashMap<String, Vec<usize>>,
    visited: &mut [bool],
    order: &mut Vec<(usize, usize)>,
) {
    if visited[index] {
        return;
    }
    visited[index] = true;
    order.push((index, depth));
    let id = sorted[index].id.to_string();
    if let Some(descendants) = children.get(&id) {
        for &child in descendants {
            visit(child, depth + 1, sorted, children, visited, order);
        }
    }
}

impl ListTable for Vec<CheckpointListRow> {
    const HEADERS: &'static [&'static str] = &["NAME", "TYPE", "CREATED", "PARENT"];

    fn table_rows(&self, opts: &RenderOptions) -> Vec<Vec<Cell>> {
        self.iter()
            .map(|row| {
                vec![
                    Cell::new(format!("{}{}", "  ".repeat(row.depth), row.name)),
                    Cell::new(row.kind_label.clone()),
                    Cell::new(row.created_local.clone()),
                    Cell::dash(
                        row.parent_id
                            .as_deref()
                            .map(|id| format::short_id(id, opts.no_trunc)),
                    ),
                ]
            })
            .collect()
    }
}

/// One line of `switch list` (spec 2.10).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwitchListRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub kind_raw: Option<u32>,
    pub uplink: Option<String>,
    #[serde(skip)]
    pub kind_label: String,
}

impl SwitchListRow {
    pub fn from_list(mut switches: Vec<SwitchSummary>) -> Vec<Self> {
        switches.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        switches.into_iter().map(Self::from).collect()
    }
}

impl From<SwitchSummary> for SwitchListRow {
    fn from(summary: SwitchSummary) -> Self {
        let (kind, value, kind_raw) = switch_kind_parts(summary.kind);
        Self {
            id: summary.id.to_string(),
            name: summary.name,
            kind: value,
            kind_raw,
            uplink: summary.uplink,
            kind_label: kind,
        }
    }
}

impl ListTable for Vec<SwitchListRow> {
    const HEADERS: &'static [&'static str] = &["NAME", "ID", "TYPE", "UPLINK"];

    fn table_rows(&self, opts: &RenderOptions) -> Vec<Vec<Cell>> {
        self.iter()
            .map(|row| {
                vec![
                    Cell::new(row.name.clone()),
                    Cell::new(format::short_id(&row.id, opts.no_trunc)),
                    Cell::new(row.kind_label.clone()),
                    Cell::dash(row.uplink.clone()),
                ]
            })
            .collect()
    }
}

/// Everything `switch get` prints (spec 2.10).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwitchDetailView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub kind_raw: Option<u32>,
    pub uplink: Option<String>,
    pub management_os_shared: bool,
    pub vlan_mode: String,
    pub vlan_mode_raw: Option<u32>,
    pub vlan_access_id: Option<u16>,
    pub vlan_native_id: Option<u16>,
    pub vlan_allowed_ids: Vec<u16>,
    pub connected_vm_ids: Vec<String>,
    #[serde(skip)]
    pub kind_label: String,
    #[serde(skip)]
    pub vlan_mode_label: String,
    /// Names of the connected VMs. Fetched for `table` only: the other formats
    /// carry the identifiers (design 4.4).
    #[serde(skip)]
    pub connected_vm_names: Vec<String>,
}

impl SwitchDetailView {
    pub fn from_detail(detail: SwitchDetail) -> Self {
        let (kind, kind_value, kind_raw) = switch_kind_parts(detail.summary.kind);
        let (mode, mode_value, vlan_mode_raw) = vlan_mode_parts(detail.vlans.mode);
        Self {
            id: detail.summary.id.to_string(),
            name: detail.summary.name,
            kind: kind_value,
            kind_raw,
            uplink: detail.summary.uplink,
            management_os_shared: detail.management_os_shared,
            vlan_mode: mode_value,
            vlan_mode_raw,
            vlan_access_id: detail.vlans.access_id,
            vlan_native_id: detail.vlans.native_id,
            vlan_allowed_ids: detail.vlans.allowed_ids,
            connected_vm_ids: detail
                .connected_vm_ids
                .iter()
                .map(|id| id.to_string())
                .collect(),
            kind_label: kind,
            vlan_mode_label: mode,
            connected_vm_names: Vec::new(),
        }
    }

    /// The connected VMs as the table shows them: names when they were
    /// resolved, identifiers otherwise.
    fn connected(&self) -> Option<String> {
        let connected = if self.connected_vm_names.is_empty() {
            &self.connected_vm_ids
        } else {
            &self.connected_vm_names
        };
        if connected.is_empty() {
            None
        } else {
            Some(connected.join(", "))
        }
    }
}

impl DetailTable for SwitchDetailView {
    fn detail_rows(&self, opts: &RenderOptions) -> Vec<(String, Cell)> {
        let allowed = if self.vlan_allowed_ids.is_empty() {
            None
        } else {
            Some(
                self.vlan_allowed_ids
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        };
        vec![
            ("Name".to_string(), Cell::new(self.name.clone())),
            (
                "Id".to_string(),
                Cell::new(format::short_id(&self.id, opts.no_trunc)),
            ),
            ("Type".to_string(), Cell::new(self.kind_label.clone())),
            ("Uplink".to_string(), Cell::dash(self.uplink.clone())),
            (
                "Management OS".to_string(),
                Cell::new(if self.management_os_shared {
                    "shared"
                } else {
                    "not shared"
                }),
            ),
            (
                "VLAN mode".to_string(),
                Cell::new(self.vlan_mode_label.clone()),
            ),
            (
                "VLAN access id".to_string(),
                Cell::dash(self.vlan_access_id.map(|id| id.to_string())),
            ),
            (
                "VLAN native id".to_string(),
                Cell::dash(self.vlan_native_id.map(|id| id.to_string())),
            ),
            ("VLAN allowed ids".to_string(), Cell::dash(allowed)),
            ("Connected VMs".to_string(), Cell::dash(self.connected())),
        ]
    }
}

/// One line of `vhd list` (spec 2.10).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VhdListRow {
    pub vm: String,
    pub vm_id: String,
    pub controller: String,
    pub controller_number: u8,
    pub controller_lun: u8,
    pub controller_raw: Option<u32>,
    pub path: String,
    /// `None` when the file could not be read: the value is not just unknown,
    /// it is missing (design 4.4).
    pub format: Option<String>,
    pub format_raw: Option<u32>,
    pub kind: Option<String>,
    pub kind_raw: Option<u32>,
    pub virtual_size_bytes: Option<u64>,
    pub file_size_bytes: Option<u64>,
    #[serde(skip)]
    pub controller_label: String,
    #[serde(skip)]
    pub format_label: String,
    #[serde(skip)]
    pub kind_label: String,
}

impl VhdListRow {
    /// Sorted by virtual machine, path, then controller slot (design 4.4).
    pub fn from_list(mut attachments: Vec<AttachedVhd>) -> Vec<Self> {
        attachments.sort_by(|a, b| {
            a.attachment
                .vm_name
                .cmp(&b.attachment.vm_name)
                .then_with(|| a.attachment.path.cmp(&b.attachment.path))
                .then_with(|| {
                    a.attachment
                        .controller
                        .number
                        .cmp(&b.attachment.controller.number)
                })
                .then_with(|| {
                    a.attachment
                        .controller
                        .lun
                        .cmp(&b.attachment.controller.lun)
                })
        });
        attachments
            .into_iter()
            .map(|item| {
                let (label, value, controller_raw) = controller_parts(item.attachment.controller);
                let (format_label, format, format_raw, kind_label, kind, kind_raw) =
                    match &item.info {
                        Some(info) => {
                            let (format_label, format, format_raw) = vhd_format_parts(info.format);
                            let (kind_label, kind, kind_raw) = vhd_kind_parts(info.kind);
                            (
                                format_label,
                                Some(format),
                                format_raw,
                                kind_label,
                                Some(kind),
                                kind_raw,
                            )
                        }
                        None => ("-".to_string(), None, None, "-".to_string(), None, None),
                    };
                Self {
                    vm: item.attachment.vm_name,
                    vm_id: item.attachment.vm_id.to_string(),
                    controller: value,
                    controller_number: item.attachment.controller.number,
                    controller_lun: item.attachment.controller.lun,
                    path: item.attachment.path.to_string_lossy().into_owned(),
                    format,
                    format_raw,
                    kind,
                    kind_raw,
                    virtual_size_bytes: item.info.as_ref().map(|info| info.virtual_size_bytes),
                    file_size_bytes: item.info.as_ref().map(|info| info.file_size_bytes),
                    controller_raw,
                    controller_label: format!(
                        "{label} {}:{}",
                        item.attachment.controller.number, item.attachment.controller.lun
                    ),
                    format_label,
                    kind_label,
                }
            })
            .collect()
    }
}

impl ListTable for Vec<VhdListRow> {
    const HEADERS: &'static [&'static str] =
        &["VM", "CONTROLLER", "PATH", "FORMAT", "TYPE", "SIZE", "FILE"];

    fn table_rows(&self, _opts: &RenderOptions) -> Vec<Vec<Cell>> {
        self.iter()
            .map(|row| {
                vec![
                    Cell::new(row.vm.clone()),
                    Cell::new(row.controller_label.clone()),
                    Cell::new(row.path.clone()),
                    Cell::new(row.format_label.clone()),
                    Cell::new(row.kind_label.clone()),
                    Cell::dash(row.virtual_size_bytes.map(format::size)),
                    Cell::dash(row.file_size_bytes.map(format::size)),
                ]
            })
            .collect()
    }
}

/// Everything `vhd get` prints (spec 2.10).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VhdDetailView {
    pub path: String,
    pub format: String,
    pub format_raw: Option<u32>,
    pub kind: String,
    pub kind_raw: Option<u32>,
    pub virtual_size_bytes: u64,
    pub file_size_bytes: u64,
    pub block_size_bytes: Option<u64>,
    pub parent_path: Option<String>,
    pub disk_id: Option<String>,
    #[serde(skip)]
    pub format_label: String,
    #[serde(skip)]
    pub kind_label: String,
}

impl VhdDetailView {
    pub fn from_info(info: VhdInfo) -> Self {
        let (format_label, format, format_raw) = vhd_format_parts(info.format);
        let (kind_label, kind, kind_raw) = vhd_kind_parts(info.kind);
        Self {
            path: info.path.to_string_lossy().into_owned(),
            format,
            format_raw,
            kind,
            kind_raw,
            virtual_size_bytes: info.virtual_size_bytes,
            file_size_bytes: info.file_size_bytes,
            block_size_bytes: info.block_size_bytes,
            parent_path: info
                .parent_path
                .map(|path| path.to_string_lossy().into_owned()),
            disk_id: info.disk_id,
            format_label,
            kind_label,
        }
    }
}

impl DetailTable for VhdDetailView {
    fn detail_rows(&self, _opts: &RenderOptions) -> Vec<(String, Cell)> {
        vec![
            ("Path".to_string(), Cell::new(self.path.clone())),
            ("Format".to_string(), Cell::new(self.format_label.clone())),
            ("Type".to_string(), Cell::new(self.kind_label.clone())),
            (
                "Virtual size".to_string(),
                Cell::new(format::size(self.virtual_size_bytes)),
            ),
            (
                "File size".to_string(),
                Cell::new(format::size(self.file_size_bytes)),
            ),
            (
                "Block size".to_string(),
                Cell::dash(self.block_size_bytes.map(format::size)),
            ),
            (
                "Parent path".to_string(),
                Cell::dash(self.parent_path.clone()),
            ),
            ("Disk id".to_string(), Cell::dash(self.disk_id.clone())),
        ]
    }
}
