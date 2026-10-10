//! Turning a classification into a message, a hint and an exit code
//! (design 4.7, spec 2.11 and 2.12).

use std::io::Write;

use hypervctl_core::HyperVError;

/// The exit code an error ends the process with (spec 2.11).
pub fn exit_code(error: &HyperVError) -> u8 {
    match error {
        HyperVError::NotFound { .. } | HyperVError::Ambiguous { .. } => 3,
        HyperVError::PermissionDenied { .. } => 4,
        HyperVError::Timeout { .. } => 5,
        _ => 1,
    }
}

/// The hint of a classification (spec 2.12). The classifications the table
/// leaves without one report `None`.
pub fn hint(error: &HyperVError) -> Option<&'static str> {
    match error {
        HyperVError::PermissionDenied { .. } => {
            Some(r#"Run as Administrator, or add your user to the "Hyper-V Administrators" group"#)
        }
        HyperVError::HyperVUnavailable { .. } => {
            Some("Enable the Hyper-V feature / check that the vmms service is running")
        }
        HyperVError::Backend { .. } => Some("Re-run with -v to see the raw provider error"),
        _ => None,
    }
}

/// Writes the error, the candidates of an ambiguous name, its hint and (only
/// with `-v`) the raw provider error (spec 2.10).
pub fn write<W>(out: &mut W, error: &HyperVError, verbose: u8) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(out, "error: {error}")?;
    if let HyperVError::Ambiguous { candidates, .. } = error {
        writeln!(out, "  candidates:")?;
        for candidate in candidates {
            writeln!(out, "    {}  {}", candidate.target, candidate.name)?;
        }
    }
    if let Some(hint) = hint(error) {
        writeln!(out, "hint: {hint}")?;
    }
    if verbose > 0 {
        if let Some(raw) = error.raw() {
            writeln!(out, "  raw: {raw}")?;
        }
    }
    Ok(())
}

/// Sends an error to stderr, where messages belong (spec 2.10).
pub fn print(error: &HyperVError, verbose: u8) {
    let mut stderr = std::io::stderr().lock();
    let _ = write(&mut stderr, error, verbose);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hypervctl_core::{
        Candidate, HyperVError, RawError, Target, TargetKind, UnavailableReason, VmId, VmState,
    };
    use uuid::Uuid;

    use super::*;

    fn not_found() -> HyperVError {
        HyperVError::NotFound {
            kind: TargetKind::Vm,
            specified: "nope".to_string(),
            raw: None,
        }
    }

    fn ambiguous() -> HyperVError {
        HyperVError::Ambiguous {
            kind: TargetKind::Vm,
            specified: "dup".to_string(),
            candidates: vec![
                Candidate {
                    target: Target::Vm(VmId::new(Uuid::from_u128(0x5 << 96))),
                    name: "dup".to_string(),
                },
                Candidate {
                    target: Target::Vm(VmId::new(Uuid::from_u128(0x6 << 96))),
                    name: "dup".to_string(),
                },
            ],
            raw: None,
        }
    }

    fn timeout() -> HyperVError {
        HyperVError::Timeout {
            elapsed: Duration::from_secs(60),
            last_state: VmState::Starting,
            raw: None,
        }
    }

    #[test]
    fn exit_codes_follow_the_table_of_the_specification() {
        assert_eq!(exit_code(&not_found()), 3);
        assert_eq!(exit_code(&ambiguous()), 3);
        assert_eq!(exit_code(&HyperVError::PermissionDenied { raw: None }), 4);
        assert_eq!(exit_code(&timeout()), 5);
        assert_eq!(exit_code(&HyperVError::backend(RawError::new("boom"))), 1);
        assert_eq!(
            exit_code(&HyperVError::HyperVUnavailable {
                reason: UnavailableReason::Unknown,
                raw: None,
            }),
            1
        );
    }

    #[test]
    fn only_the_classifications_of_the_table_have_a_hint() {
        assert!(hint(&HyperVError::PermissionDenied { raw: None }).is_some());
        assert!(hint(&HyperVError::HyperVUnavailable {
            reason: UnavailableReason::ServiceStopped,
            raw: None,
        })
        .is_some());
        assert!(hint(&HyperVError::backend(RawError::new("boom"))).is_some());
        assert_eq!(hint(&not_found()), None);
        assert_eq!(hint(&timeout()), None);
    }

    #[test]
    fn an_error_reports_message_candidates_and_hint() {
        let mut out = Vec::new();
        write(&mut out, &ambiguous(), 0)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.starts_with("error: VM 'dup' is ambiguous\n"),
            "{text:?}"
        );
        assert!(text.contains("  candidates:\n"), "{text:?}");
        assert!(
            text.contains("    00000005-0000-0000-0000-000000000000  dup\n"),
            "{text:?}"
        );
        assert!(!text.contains("hint:"), "{text:?}");
    }

    #[test]
    fn the_raw_provider_error_only_appears_with_verbose() {
        let error = HyperVError::PermissionDenied {
            raw: Some(RawError::with_hresult(-2147024891, "denied")),
        };
        let mut quiet = Vec::new();
        write(&mut quiet, &error, 0)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        let quiet = String::from_utf8_lossy(&quiet).into_owned();
        assert!(quiet.contains("hint: Run as Administrator"), "{quiet:?}");
        assert!(!quiet.contains("raw:"), "{quiet:?}");

        let mut loud = Vec::new();
        write(&mut loud, &error, 1)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        let loud = String::from_utf8_lossy(&loud).into_owned();
        assert!(loud.contains("  raw: 0x80070005: denied"), "{loud:?}");
    }
}
