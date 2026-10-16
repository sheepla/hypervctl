//! The action menu: which items a VM or a checkpoint offers and which of them
//! can run right now (design 5.7, spec 3.4). Every decision comes from the one
//! availability table of the core, so the menu and the CLI can never disagree
//! (spec 2.9, anomaly 19). Pure functions; the table tests are the
//! specification table itself.

use hypervctl_core::{availability, states_where_ok, Availability, OpKind, VmOp, VmState};

/// One row of the VM menu, in the order the specification lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmMenuItem {
    Start,
    Stop,
    StopForce,
    Restart,
    RestartForce,
    Save,
    Pause,
    Resume,
    CreateCheckpoint,
}

/// Every VM menu item, top to bottom (spec 3.4).
pub const VM_MENU: &[VmMenuItem] = &[
    VmMenuItem::Start,
    VmMenuItem::Stop,
    VmMenuItem::StopForce,
    VmMenuItem::Restart,
    VmMenuItem::RestartForce,
    VmMenuItem::Save,
    VmMenuItem::Pause,
    VmMenuItem::Resume,
    VmMenuItem::CreateCheckpoint,
];

/// One row of the checkpoint menu (spec 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointMenuItem {
    Restore,
    Remove,
}

/// Every checkpoint menu item, top to bottom.
pub const CHECKPOINT_MENU: &[CheckpointMenuItem] =
    &[CheckpointMenuItem::Restore, CheckpointMenuItem::Remove];

impl VmMenuItem {
    /// The label on the menu (spec 3.4 spells them out).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::StopForce => "Stop (force)",
            Self::Restart => "Restart",
            Self::RestartForce => "Restart (force)",
            Self::Save => "Save",
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::CreateCheckpoint => "Create checkpoint",
        }
    }

    /// Whether the item asks for a confirmation before it runs (spec 3.4).
    pub const fn needs_confirm(self) -> bool {
        matches!(self, Self::StopForce | Self::RestartForce)
    }

    /// The operation behind the item, as the runner executes it. `Create
    /// checkpoint` is dispatched by the reducer itself, so its arm is
    /// unreachable and holds a harmless placeholder.
    pub const fn op(self) -> VmOp {
        match self {
            Self::Start => VmOp::Start,
            Self::Stop => VmOp::Stop,
            Self::StopForce => VmOp::StopForce,
            Self::Restart => VmOp::Restart,
            Self::RestartForce => VmOp::RestartForce,
            Self::Save => VmOp::Save,
            Self::Pause => VmOp::Pause,
            Self::Resume => VmOp::Resume,
            // Never reached: menu_select dispatches CreateCheckpoint as an
            // OpRequest::CreateCheckpoint, not through op().
            Self::CreateCheckpoint => VmOp::Start,
        }
    }

    /// The operation behind the item, as the availability table names it.
    /// `Create checkpoint` is not a state change; it still has its own row
    /// there, so the menu can derive from the same table (spec 2.9).
    pub const fn op_kind(self) -> OpKind {
        match self {
            Self::Start => OpKind::Start,
            Self::Stop => OpKind::Stop,
            Self::StopForce => OpKind::StopForce,
            Self::Restart => OpKind::Restart,
            Self::RestartForce => OpKind::RestartForce,
            Self::Save => OpKind::Save,
            Self::Pause => OpKind::Pause,
            Self::Resume => OpKind::Resume,
            Self::CreateCheckpoint => OpKind::CheckpointCreate,
        }
    }
}

impl CheckpointMenuItem {
    /// The label on the menu.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Restore => "Restore",
            Self::Remove => "Remove",
        }
    }

    /// Checkpoint operations are always confirmed (spec 3.4).
    pub const fn needs_confirm(self) -> bool {
        true
    }

    /// The operation behind the item, as the availability table names it.
    pub const fn op_kind(self) -> OpKind {
        match self {
            Self::Restore => OpKind::CheckpointRestore,
            Self::Remove => OpKind::CheckpointRemove,
        }
    }
}

/// Whether each VM menu item can run in `state` right now (spec 3.4, design
/// 5.7): exactly the cells the availability table calls `Ok`, with an
/// operation in flight disabling everything (anomaly 19: the menu is the
/// table, nothing else).
pub fn vm_item_enabled(item: VmMenuItem, state: VmState, op_in_flight: bool) -> bool {
    !op_in_flight && availability(item.op_kind(), state) == Availability::Ok
}

/// Whether a checkpoint menu item can run: the same table, plus a checkpoint
/// being selected at all (spec 3.4, anomaly 21).
pub fn checkpoint_item_enabled(
    item: CheckpointMenuItem,
    state: Option<VmState>,
    checkpoint_selected: bool,
    op_in_flight: bool,
) -> bool {
    let Some(state) = state else {
        return false;
    };
    !op_in_flight && checkpoint_selected && availability(item.op_kind(), state) == Availability::Ok
}

/// The states in which `item` can run, as the menu shows them next to a grey
/// row (spec 3.4). Straight from the table; no second list exists (design 5.7).
pub fn enabled_states_text(item_op: OpKind) -> String {
    states_where_ok(item_op)
        .into_iter()
        .map(crate::cli::output::views::state_label)
        .collect::<Vec<_>>()
        .join("/")
}

/// The question a VM confirmation dialog asks (spec 3.4).
pub fn vm_confirm_question(item: VmMenuItem, vm_name: &str) -> String {
    match item {
        VmMenuItem::StopForce => format!("Force-stop {vm_name}?"),
        VmMenuItem::RestartForce => format!("Force-restart {vm_name}?"),
        other => format!("Run {} on {vm_name}?", other.label().to_lowercase()),
    }
}

/// The question a checkpoint confirmation dialog asks (spec 3.4).
pub fn checkpoint_confirm_question(
    item: CheckpointMenuItem,
    vm_name: &str,
    checkpoint_name: &str,
) -> String {
    match item {
        CheckpointMenuItem::Restore => {
            format!("Restore '{checkpoint_name}' of {vm_name}?")
        }
        CheckpointMenuItem::Remove => {
            format!("Remove '{checkpoint_name}' of {vm_name}?")
        }
    }
}
