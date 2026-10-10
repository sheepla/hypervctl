//! Backend-agnostic error classification.
//!
//! The backend maps its raw provider errors onto these variants; the frontend
//! turns a variant into a message, a hint and an exit code (spec 2.11/2.12).
//! The raw provider error is carried along so that `-v` can show it.

use std::fmt;
use std::time::Duration;

use thiserror::Error;

use crate::target::{Candidate, TargetKind};
use crate::vm::VmState;

/// The raw provider error, shown only with `-v`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawError {
    /// HRESULT as reported by the provider, when there is one.
    pub hresult: Option<i32>,
    /// Provider message. May be localized, so it is never parsed.
    pub message: String,
}

impl RawError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            hresult: None,
            message: message.into(),
        }
    }

    pub fn with_hresult(hresult: i32, message: impl Into<String>) -> Self {
        Self {
            hresult: Some(hresult),
            message: message.into(),
        }
    }
}

impl fmt::Display for RawError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.hresult {
            Some(code) => {
                write!(f, "{:#010X}", code as u32)?;
                if !self.message.is_empty() {
                    write!(f, ": {}", self.message)?;
                }
                Ok(())
            }
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for RawError {}

/// Why Hyper-V could not be reached at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnavailableReason {
    /// `root\virtualization\v2` does not exist: the Hyper-V feature is missing
    /// or the service did not register its namespace.
    NamespaceMissing,
    /// The namespace exists but the service does not answer: vmms is stopped.
    ServiceStopped,
    /// Could not tell which of the two it is.
    Unknown,
}

impl fmt::Display for UnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamespaceMissing => {
                f.write_str(r"the namespace root\virtualization\v2 is missing")
            }
            Self::ServiceStopped => f.write_str("the Hyper-V service (vmms) is not responding"),
            Self::Unknown => f.write_str("the cause is unknown"),
        }
    }
}

/// An error, classified for the frontend.
#[derive(Debug, Clone, Error, PartialEq)]
pub enum HyperVError {
    #[error("permission denied")]
    PermissionDenied { raw: Option<RawError> },

    #[error("Hyper-V is unavailable: {reason}")]
    HyperVUnavailable {
        reason: UnavailableReason,
        raw: Option<RawError>,
    },

    #[error("{kind} not found: '{specified}'")]
    NotFound {
        kind: TargetKind,
        specified: String,
        raw: Option<RawError>,
    },

    #[error("{kind} '{specified}' is ambiguous")]
    Ambiguous {
        kind: TargetKind,
        specified: String,
        candidates: Vec<Candidate>,
        raw: Option<RawError>,
    },

    #[error("operation '{operation}' is not valid in state {state}")]
    InvalidState {
        target: Option<crate::ids::VmId>,
        state: VmState,
        operation: String,
        raw: Option<RawError>,
    },

    #[error("timed out after {elapsed:?} (last observed state: {last_state})")]
    Timeout {
        elapsed: Duration,
        last_state: VmState,
        raw: Option<RawError>,
    },

    #[error("Hyper-V job {code} failed: {message}")]
    JobFailed {
        code: u32,
        message: String,
        raw: Option<RawError>,
    },

    #[error("backend error: {raw}")]
    Backend { raw: RawError },
}

impl HyperVError {
    /// An error whose cause is unknown and that only `-v` can explain further.
    pub fn backend(raw: RawError) -> Self {
        Self::Backend { raw }
    }

    /// The raw provider error behind this classification, when there is one.
    pub fn raw(&self) -> Option<&RawError> {
        match self {
            Self::PermissionDenied { raw }
            | Self::HyperVUnavailable { raw, .. }
            | Self::NotFound { raw, .. }
            | Self::Ambiguous { raw, .. }
            | Self::InvalidState { raw, .. }
            | Self::Timeout { raw, .. }
            | Self::JobFailed { raw, .. } => raw.as_ref(),
            Self::Backend { raw } => Some(raw),
        }
    }

    /// The classification as a stable, machine-readable name, for result rows
    /// and the TUI (design 4.4).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PermissionDenied { .. } => "permission_denied",
            Self::HyperVUnavailable { .. } => "hyper_v_unavailable",
            Self::NotFound { .. } => "not_found",
            Self::Ambiguous { .. } => "ambiguous",
            Self::InvalidState { .. } => "invalid_state",
            Self::Timeout { .. } => "timeout",
            Self::JobFailed { .. } => "job_failed",
            Self::Backend { .. } => "backend",
        }
    }

    /// Whether this error says the backend actor stopped, which the TUI treats
    /// as fatal (design 5.5).
    pub fn is_backend_stopped(&self) -> bool {
        match self {
            Self::Backend { raw } => {
                raw.message == crate::actor::STOPPED_MESSAGE
                    || raw.message == crate::actor::UNREACHABLE_MESSAGE
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::Target;

    #[test]
    fn raw_error_shows_the_hresult_in_hex() {
        // -2147217405 == 0x80041003 (WBEM_E_ACCESS_DENIED)
        let denied = RawError::with_hresult(-2147217405, "access denied");
        assert_eq!(denied.to_string(), "0x80041003: access denied");
        let plain = RawError::new("something went wrong");
        assert_eq!(plain.to_string(), "something went wrong");
        // -2147024891 == 0x80070005 (E_ACCESSDENIED); an empty message drops the separator
        assert_eq!(
            RawError::with_hresult(-2147024891, "").to_string(),
            "0x80070005"
        );
    }

    #[test]
    fn messages_carry_the_classification() {
        let not_found = HyperVError::NotFound {
            kind: TargetKind::Vm,
            specified: "web-01".to_string(),
            raw: None,
        };
        assert_eq!(not_found.to_string(), "VM not found: 'web-01'");
        assert_eq!(not_found.raw(), None);

        let ambiguous = HyperVError::Ambiguous {
            kind: TargetKind::Vm,
            specified: "web".to_string(),
            candidates: vec![Candidate {
                target: Target::Vm(crate::ids::VmId::new(uuid::Uuid::nil())),
                name: "web".to_string(),
            }],
            raw: None,
        };
        assert_eq!(ambiguous.to_string(), "VM 'web' is ambiguous");

        let invalid = HyperVError::InvalidState {
            target: None,
            state: VmState::Running,
            operation: "restore_checkpoint".to_string(),
            raw: None,
        };
        assert_eq!(
            invalid.to_string(),
            "operation 'restore_checkpoint' is not valid in state Running"
        );

        let timeout = HyperVError::Timeout {
            elapsed: Duration::from_secs(30),
            last_state: VmState::Off,
            raw: None,
        };
        assert!(timeout.to_string().contains("30s"));

        let unavailable = HyperVError::HyperVUnavailable {
            reason: UnavailableReason::NamespaceMissing,
            raw: None,
        };
        assert_eq!(
            unavailable.to_string(),
            r"Hyper-V is unavailable: the namespace root\virtualization\v2 is missing"
        );
    }

    #[test]
    fn backend_error_is_reachable_through_raw() {
        let error = HyperVError::backend(RawError::with_hresult(-2147024891, "denied"));
        assert_eq!(error.raw().map(|r| r.hresult), Some(Some(-2147024891)));
        assert_eq!(error.to_string(), "backend error: 0x80070005: denied");
    }

    #[test]
    fn job_failure_keeps_its_code() {
        let error = HyperVError::JobFailed {
            code: 32770,
            message: "not supported".to_string(),
            raw: None,
        };
        assert_eq!(error.to_string(), "Hyper-V job 32770 failed: not supported");
    }

    #[test]
    fn only_the_actor_stop_is_reported_as_such() {
        assert!(crate::actor::stopped_error().is_backend_stopped());
        assert!(crate::actor::unreachable_error().is_backend_stopped());
        assert!(!HyperVError::backend(RawError::new("something else")).is_backend_stopped());
        assert!(!HyperVError::PermissionDenied { raw: None }.is_backend_stopped());
    }
}
