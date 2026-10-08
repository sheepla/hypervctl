//! VM aggregate types.

use std::fmt;
use std::time::Duration;

use crate::ids::VmId;
use crate::vhd::VhdAttachment;

/// Operating state of a VM.
///
/// Stable states are `Running`, `Off`, `Saved` and `Paused`. The remaining
/// variants are transitional: the VM is moving towards a stable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VmState {
    Running,
    Off,
    Saved,
    Paused,
    Starting,
    Stopping,
    Saving,
    Pausing,
    Resuming,
    Resetting,
    /// A value the backend does not know. The raw provider value is kept instead
    /// of being dropped, so it can be reported as `Unknown(raw)`.
    Unknown(u32),
}

impl VmState {
    /// Maps Hyper-V's `EnabledState` value to a state.
    ///
    /// The table below is the one verified on real hardware during M0 (design
    /// document 3.7.1). Value `1` means "enabled but not settled yet" and only
    /// says that a transition is in progress; without the locale-dependent
    /// `OtherEnabledState` the target state cannot be told, so it is kept raw.
    pub const fn from_enabled_state(raw: u32) -> Self {
        match raw {
            1 => Self::Unknown(1),
            2 => Self::Running,
            3 => Self::Off,
            4 => Self::Stopping,
            6 => Self::Saved,
            9 => Self::Paused,
            10 => Self::Starting,
            other => Self::Unknown(other),
        }
    }

    /// `true` while the VM is moving between stable states.
    pub const fn is_transition(self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::Stopping
                | Self::Saving
                | Self::Pausing
                | Self::Resuming
                | Self::Resetting
        )
    }

    /// The provider value when, and only when, the state is [`VmState::Unknown`].
    pub const fn raw_if_unknown(self) -> Option<u32> {
        match self {
            Self::Unknown(raw) => Some(raw),
            _ => None,
        }
    }
}

impl fmt::Display for VmState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("Running"),
            Self::Off => f.write_str("Off"),
            Self::Saved => f.write_str("Saved"),
            Self::Paused => f.write_str("Paused"),
            Self::Starting => f.write_str("Starting"),
            Self::Stopping => f.write_str("Stopping"),
            Self::Saving => f.write_str("Saving"),
            Self::Pausing => f.write_str("Pausing"),
            Self::Resuming => f.write_str("Resuming"),
            Self::Resetting => f.write_str("Resetting"),
            Self::Unknown(raw) => write!(f, "Unknown({raw})"),
        }
    }
}

/// Everything `vm list` needs about a VM, fetched in one call.
#[derive(Debug, Clone, PartialEq)]
pub struct VmSummary {
    pub id: VmId,
    pub name: String,
    pub state: VmState,
    /// `None` when the provider reports no usage (for example an `Off` VM).
    pub cpu_usage_percent: Option<f64>,
    pub memory_assigned_bytes: Option<u64>,
    pub uptime: Option<Duration>,
    pub generation: Option<u8>,
}

/// Everything `vm get` needs.
#[derive(Debug, Clone, PartialEq)]
pub struct VmDetail {
    pub summary: VmSummary,
    pub processor_count: u32,
    pub memory: MemoryConfig,
    pub disks: Vec<VhdAttachment>,
    pub nics: Vec<NicConfig>,
    pub checkpoint_count: u32,
    /// Location of the configuration file.
    pub config_path: String,
    pub integration_services: Vec<IntegrationService>,
    pub notes: Option<String>,
}

/// Startup and dynamic memory settings. Missing limits stay `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryConfig {
    pub startup_bytes: u64,
    pub dynamic_enabled: bool,
    pub min_bytes: Option<u64>,
    pub max_bytes: Option<u64>,
}

/// A network adapter as reported for `vm get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NicConfig {
    /// Name of the connected switch; `None` when the adapter is disconnected.
    pub switch_name: Option<String>,
    pub mac_address: Option<String>,
    pub ip_addresses: Vec<std::net::IpAddr>,
}

/// Guest integration service as reported for `vm get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationService {
    pub name: String,
    pub enabled: bool,
    pub status: IntegrationStatus,
}

/// Coarse health of an integration service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntegrationStatus {
    Operational,
    Error,
    /// Value the backend did not map; kept so it can be shown instead of dropped.
    Unknown(u16),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_values_verified_in_m0() {
        assert_eq!(VmState::from_enabled_state(2), VmState::Running);
        assert_eq!(VmState::from_enabled_state(3), VmState::Off);
        assert_eq!(VmState::from_enabled_state(4), VmState::Stopping);
        assert_eq!(VmState::from_enabled_state(6), VmState::Saved);
        assert_eq!(VmState::from_enabled_state(9), VmState::Paused);
        assert_eq!(VmState::from_enabled_state(10), VmState::Starting);
    }

    #[test]
    fn keeps_unknown_values_instead_of_failing() {
        assert_eq!(VmState::from_enabled_state(1), VmState::Unknown(1));
        assert_eq!(VmState::from_enabled_state(42), VmState::Unknown(42));
        assert_eq!(VmState::Unknown(42).raw_if_unknown(), Some(42));
        assert_eq!(VmState::Running.raw_if_unknown(), None);
        assert_eq!(VmState::Unknown(42).to_string(), "Unknown(42)");
    }

    #[test]
    fn marks_transitional_states() {
        for state in [
            VmState::Starting,
            VmState::Stopping,
            VmState::Saving,
            VmState::Pausing,
            VmState::Resuming,
            VmState::Resetting,
        ] {
            assert!(state.is_transition(), "{state} should be transitional");
        }
        for state in [
            VmState::Running,
            VmState::Off,
            VmState::Saved,
            VmState::Paused,
        ] {
            assert!(!state.is_transition(), "{state} should be stable");
        }
        assert!(!VmState::Unknown(1).is_transition());
    }

    #[test]
    fn missing_measurements_stay_none() {
        let summary = VmSummary {
            id: VmId::new(uuid::Uuid::nil()),
            name: "test".to_string(),
            state: VmState::Off,
            cpu_usage_percent: None,
            memory_assigned_bytes: None,
            uptime: None,
            generation: None,
        };
        assert_eq!(summary.cpu_usage_percent, None);
        assert_eq!(summary.uptime, None);
        assert_ne!(summary.cpu_usage_percent, Some(0.0));
    }
}
