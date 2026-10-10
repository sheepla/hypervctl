//! What is stacked on top of the panes: the action menu, the confirmation
//! dialog or the help (design 5.3). The menu and the confirmation fix their
//! target when they open, so a refresh under them never retargets them
//! (spec 3.6).

use hypervctl_core::{CheckpointId, VmId};

use super::menu::VmMenuItem;

/// What, if anything, covers the panes (design 5.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    None,
    Menu(MenuState),
    Confirm(ConfirmState),
    Help,
}

impl Overlay {
    /// The VM a popup has fixed, when it has one (spec 3.6).
    pub fn target(&self) -> Option<VmId> {
        match self {
            Self::Menu(menu) => Some(menu.vm),
            Self::Confirm(confirm) => Some(confirm.vm),
            Self::None | Self::Help => None,
        }
    }

    /// The open menu, when that is what is stacked on top.
    pub fn menu(&self) -> Option<&MenuState> {
        match self {
            Self::Menu(menu) => Some(menu),
            _ => None,
        }
    }

    /// The open confirmation dialog, when that is what is stacked on top.
    pub fn confirm(&self) -> Option<&ConfirmState> {
        match self {
            Self::Confirm(confirm) => Some(confirm),
            _ => None,
        }
    }
}

/// Which menu is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Vm,
    Checkpoint,
}

/// The open action menu (design 5.3).
#[derive(Debug, Clone, PartialEq)]
pub struct MenuState {
    /// The VM the menu was opened for; it does not follow the selection.
    pub vm: VmId,
    pub kind: MenuKind,
    /// For the checkpoint menu: which checkpoint (design 5.6).
    pub checkpoint: Option<(CheckpointId, String)>,
    /// The highlighted row: a [`VmMenuItem`] or a [`CheckpointMenuItem`] index.
    pub cursor: usize,
}

/// The open confirmation dialog (spec 3.4).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmState {
    /// The VM the operation was confirmed for; it does not follow anything.
    pub vm: VmId,
    pub question: String,
    /// What runs when the user presses `y`.
    pub request: OpRequest,
}

/// An operation the user picked in a menu (design 5.4). The names travel with
/// it so that the result message needs no extra lookup.
#[derive(Debug, Clone, PartialEq)]
pub enum OpRequest {
    Vm {
        vm: VmId,
        vm_name: String,
        op: VmMenuItem,
    },
    CreateCheckpoint {
        vm: VmId,
        vm_name: String,
    },
    RestoreCheckpoint {
        vm: VmId,
        vm_name: String,
        checkpoint: CheckpointId,
        checkpoint_name: String,
    },
    RemoveCheckpoint {
        vm: VmId,
        vm_name: String,
        checkpoint: CheckpointId,
        checkpoint_name: String,
    },
}

impl OpRequest {
    /// The VM the operation runs on.
    pub const fn vm(&self) -> VmId {
        match self {
            Self::Vm { vm, .. }
            | Self::CreateCheckpoint { vm, .. }
            | Self::RestoreCheckpoint { vm, .. }
            | Self::RemoveCheckpoint { vm, .. } => *vm,
        }
    }
}
