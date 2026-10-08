//! Virtual switch types.

use crate::ids::{SwitchId, VmId};

/// Everything `switch list` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchSummary {
    pub id: SwitchId,
    pub name: String,
    pub kind: SwitchKind,
    /// Physical NIC the external switch bridges; `None` for other kinds.
    pub uplink: Option<String>,
}

/// Everything `switch get` adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchDetail {
    pub summary: SwitchSummary,
    pub management_os_shared: bool,
    pub vlans: VlanSettings,
    pub connected_vm_ids: Vec<VmId>,
}

/// Kind of a virtual switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwitchKind {
    External,
    Internal,
    Private,
    /// Value the backend did not map; kept so it can be shown instead of dropped.
    Unknown(u32),
}

/// VLAN configuration of a switch. Missing identifiers stay `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VlanSettings {
    pub mode: VlanMode,
    pub access_id: Option<u16>,
    pub native_id: Option<u16>,
    pub allowed_ids: Vec<u16>,
}

/// VLAN mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VlanMode {
    Disabled,
    Access,
    Trunk,
    Native,
    /// Value the backend did not map; kept so it can be shown instead of dropped.
    Unknown(u32),
}

impl Default for VlanSettings {
    fn default() -> Self {
        Self {
            mode: VlanMode::Disabled,
            access_id: None,
            native_id: None,
            allowed_ids: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn switch_without_vlan_has_no_identifiers() {
        let vlans = VlanSettings::default();
        assert_eq!(vlans.mode, VlanMode::Disabled);
        assert_eq!(vlans.access_id, None);
        assert_eq!(vlans.native_id, None);
        assert!(vlans.allowed_ids.is_empty());
    }

    #[test]
    fn private_switch_has_no_uplink() {
        let summary = SwitchSummary {
            id: SwitchId::new(Uuid::nil()),
            name: "test-priv".to_string(),
            kind: SwitchKind::Private,
            uplink: None,
        };
        assert_eq!(summary.uplink, None);
        assert_eq!(summary.kind, SwitchKind::Private);
    }
}
