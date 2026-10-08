//! The thing a command is about: which kind of object, and (when resolved) which
//! object exactly.

use std::fmt;
use std::path::PathBuf;

use crate::ids::{CheckpointId, SwitchId, VmId};

/// Kind of object a command targets. Used in error messages and to resolve a
/// name against the right list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Vm,
    Checkpoint,
    Switch,
    Vhd,
}

impl TargetKind {
    pub const ALL: [TargetKind; 4] = [
        TargetKind::Vm,
        TargetKind::Checkpoint,
        TargetKind::Switch,
        TargetKind::Vhd,
    ];
}

impl fmt::Display for TargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vm => f.write_str("VM"),
            Self::Checkpoint => f.write_str("checkpoint"),
            Self::Switch => f.write_str("switch"),
            Self::Vhd => f.write_str("VHD"),
        }
    }
}

/// A resolved target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Vm(VmId),
    Checkpoint(CheckpointId),
    Switch(SwitchId),
    Vhd(PathBuf),
}

impl Target {
    pub const fn kind(&self) -> TargetKind {
        match self {
            Self::Vm(_) => TargetKind::Vm,
            Self::Checkpoint(_) => TargetKind::Checkpoint,
            Self::Switch(_) => TargetKind::Switch,
            Self::Vhd(_) => TargetKind::Vhd,
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vm(id) => id.fmt(f),
            Self::Checkpoint(id) => id.fmt(f),
            Self::Switch(id) => id.fmt(f),
            Self::Vhd(path) => write!(f, "{}", path.display()),
        }
    }
}

/// One of the objects a name could mean, reported when a name is ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub target: Target,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn reports_the_kind_of_a_resolved_target() {
        assert_eq!(Target::Vm(VmId::new(Uuid::nil())).kind(), TargetKind::Vm);
        assert_eq!(
            Target::Checkpoint(CheckpointId::new(Uuid::nil())).kind(),
            TargetKind::Checkpoint
        );
        assert_eq!(
            Target::Switch(SwitchId::new(Uuid::nil())).kind(),
            TargetKind::Switch
        );
        assert_eq!(
            Target::Vhd(PathBuf::from(r"C:\vhds\disk.vhdx")).kind(),
            TargetKind::Vhd
        );
    }

    #[test]
    fn displays_identifiers_and_paths() {
        let id = VmId::new(Uuid::nil());
        assert_eq!(
            Target::Vm(id).to_string(),
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            Target::Vhd(PathBuf::from("disk.vhdx")).to_string(),
            "disk.vhdx"
        );
    }

    #[test]
    fn every_kind_is_listed() {
        assert_eq!(TargetKind::ALL.len(), 4);
    }
}
