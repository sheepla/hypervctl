//! The action menu: which items a VM or a checkpoint offers and which of them
//! can run right now (design 5.7, spec 3.4). Pure functions; the table tests
//! are the specification table itself.

use hypervctl_core::{VmOp, VmState};

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

    /// The operation behind the item. `Create checkpoint` is not a state
    /// change and is dispatched by the reducer itself.
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
            Self::CreateCheckpoint => VmOp::Start,
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
}

/// Whether each VM menu item can run in `state` right now (spec 3.4, design
/// 5.7): the table of the specification, with `Unknown` and an operation in
/// flight disabling everything.
pub fn vm_item_enabled(item: VmMenuItem, state: VmState, op_in_flight: bool) -> bool {
    if op_in_flight || matches!(state, VmState::Unknown(_)) {
        return false;
    }
    use VmMenuItem::*;
    match item {
        Start => matches!(state, VmState::Off | VmState::Saved | VmState::Paused),
        Stop => state == VmState::Running,
        StopForce => matches!(state, VmState::Running | VmState::Paused),
        Restart | RestartForce => state == VmState::Running,
        Save | Pause => state == VmState::Running,
        Resume => state == VmState::Paused,
        CreateCheckpoint => matches!(
            state,
            VmState::Running | VmState::Off | VmState::Saved | VmState::Paused
        ),
    }
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
