//! HRESULT classification shared by the WMI backend (design 3.7.9).
//!
//! Pure functions with no WMI dependency so the mapping is unit-tested on any
//! target. Codes verified during M0 are listed in `docs/hypervctl-m0-findings.md`.

use hypervctl_core::{HyperVError, RawError, UnavailableReason};

/// `WBEM_E_ACCESS_DENIED`
const WBEM_E_ACCESS_DENIED: i32 = 0x8004_1003_u32 as i32;
/// `WBEM_E_INVALID_NAMESPACE`
const WBEM_E_INVALID_NAMESPACE: i32 = 0x8004_100E_u32 as i32;
/// `E_ACCESSDENIED`
const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;
/// `RPC_S_SERVER_UNAVAILABLE`
const RPC_S_SERVER_UNAVAILABLE: i32 = 0x8007_06BA_u32 as i32;
/// `RPC_S_CALL_FAILED`
const RPC_S_CALL_FAILED: i32 = 0x8007_06BE_u32 as i32;

/// Maps a raw HRESULT onto the shared error classification.
///
/// Only codes whose meaning is certain become a dedicated variant; everything
/// else stays a [`HyperVError::Backend`] so a wrong guess never misleads the
/// operator (design 3.7.9, M0 findings item 8).
pub fn classify_hresult(hresult: i32) -> HyperVError {
    let raw = RawError::with_hresult(hresult, "");
    match hresult {
        WBEM_E_ACCESS_DENIED | E_ACCESSDENIED => HyperVError::PermissionDenied { raw: Some(raw) },
        WBEM_E_INVALID_NAMESPACE => HyperVError::HyperVUnavailable {
            reason: UnavailableReason::NamespaceMissing,
            raw: Some(raw),
        },
        RPC_S_SERVER_UNAVAILABLE | RPC_S_CALL_FAILED => HyperVError::HyperVUnavailable {
            reason: UnavailableReason::ServiceStopped,
            raw: Some(raw),
        },
        _ => HyperVError::backend(raw),
    }
}

/// Maps a failed connection attempt (no HRESULT, or a WMI wrapper error)
/// onto the shared classification. Used when `root\virtualization\v2` cannot
/// be opened at all.
pub fn classify_connect_failure(message: impl Into<String>) -> HyperVError {
    HyperVError::HyperVUnavailable {
        reason: UnavailableReason::Unknown,
        raw: Some(RawError::new(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_denied_codes_become_permission_denied() {
        let error = classify_hresult(WBEM_E_ACCESS_DENIED);
        assert_eq!(error.kind(), "permission_denied");
        assert_eq!(
            error.raw().and_then(|raw| raw.hresult),
            Some(WBEM_E_ACCESS_DENIED)
        );

        let error = classify_hresult(E_ACCESSDENIED);
        assert_eq!(error.kind(), "permission_denied");
    }

    #[test]
    fn a_missing_namespace_is_reported_as_unavailable() {
        let error = classify_hresult(WBEM_E_INVALID_NAMESPACE);
        assert_eq!(error.kind(), "hyper_v_unavailable");
        assert!(matches!(
            error,
            HyperVError::HyperVUnavailable {
                reason: UnavailableReason::NamespaceMissing,
                ..
            }
        ));
    }

    #[test]
    fn a_stopped_service_is_reported_as_unavailable() {
        let error = classify_hresult(RPC_S_SERVER_UNAVAILABLE);
        assert!(matches!(
            error,
            HyperVError::HyperVUnavailable {
                reason: UnavailableReason::ServiceStopped,
                ..
            }
        ));
        let error = classify_hresult(RPC_S_CALL_FAILED);
        assert_eq!(error.kind(), "hyper_v_unavailable");
    }

    #[test]
    fn unknown_codes_stay_backend_errors() {
        // 0x80041002 (NOT_FOUND) has no dedicated variant: the caller decides
        // what "not found" means for the target kind.
        let error = classify_hresult(0x8004_1002_u32 as i32);
        assert_eq!(error.kind(), "backend");
        assert_eq!(
            error.raw().and_then(|raw| raw.hresult),
            Some(0x8004_1002_u32 as i32)
        );
    }

    #[test]
    fn a_connect_failure_without_a_code_is_unavailable() {
        let error = classify_connect_failure("cannot open root\\virtualization\\v2");
        assert_eq!(error.kind(), "hyper_v_unavailable");
        assert!(error.raw().is_some());
    }
}
