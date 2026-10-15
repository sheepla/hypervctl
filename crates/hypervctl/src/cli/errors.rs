//! Turning a classification into a message, a hint and an exit code
//! (design 4.7, spec 2.11 and 2.12).

use std::io::Write;

use hypervctl_core::HyperVError;

/// Everything that ends a command before it has results to show.
#[derive(Debug, PartialEq)]
pub enum CliError {
    /// A backend error, classified as usual (spec 2.12).
    Backend(HyperVError),
    /// The command line was used wrongly; exit code 2 (spec 2.11).
    Usage(String),
    /// The user answered `n` to a confirmation prompt (design 4.6).
    Aborted,
    /// Ctrl+C interrupted a wait; whatever the backend accepted keeps running
    /// (design 4.2).
    Interrupted,
}

impl From<HyperVError> for CliError {
    fn from(error: HyperVError) -> Self {
        Self::Backend(error)
    }
}

impl CliError {
    /// A usage error, ready to be returned (spec 2.11).
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }
}

/// The exit code a CLI error ends the process with (spec 2.11).
pub fn exit_code(error: &CliError) -> u8 {
    match error {
        CliError::Backend(error) => backend_exit_code(error),
        CliError::Usage(_) => 2,
        CliError::Aborted | CliError::Interrupted => 1,
    }
}

/// The exit code of one classification, when the failure stands for the whole
/// command (spec 2.11).
pub fn backend_exit_code(error: &HyperVError) -> u8 {
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

/// Writes a backend error, the candidates of an ambiguous name, its hint and
/// (only with `-v`) the raw provider error (spec 2.10).
pub fn write_backend<W>(out: &mut W, error: &HyperVError, verbose: u8) -> std::io::Result<()>
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

/// Writes one error line per failed target and each distinct hint once
/// (design 4.7), the way a command with several targets reports its failures.
pub fn write_failures<W>(
    out: &mut W,
    failures: &[(String, HyperVError)],
    verbose: u8,
) -> std::io::Result<()>
where
    W: Write,
{
    for (target, error) in failures {
        writeln!(out, "error: {target}: {error}")?;
    }
    let mut hints: Vec<&'static str> = Vec::new();
    for (_, error) in failures {
        if let Some(hint) = hint(error) {
            if !hints.contains(&hint) {
                hints.push(hint);
            }
        }
    }
    for hint in hints {
        writeln!(out, "hint: {hint}")?;
    }
    if verbose > 0 {
        for (target, error) in failures {
            if let Some(raw) = error.raw() {
                writeln!(out, "  raw ({target}): {raw}")?;
            }
        }
    }
    Ok(())
}

/// Writes a CLI error to stderr, where messages belong (spec 2.10).
pub fn write<W>(out: &mut W, error: &CliError, verbose: u8) -> std::io::Result<()>
where
    W: Write,
{
    match error {
        CliError::Backend(error) => write_backend(out, error, verbose),
        CliError::Usage(message) => writeln!(out, "error: {message}"),
        CliError::Aborted => writeln!(out, "error: aborted"),
        CliError::Interrupted => writeln!(
            out,
            "error: interrupted; the job continues to run in Hyper-V"
        ),
    }
}

/// Sends an error to stderr, where messages belong (spec 2.10).
pub fn print(error: &CliError, verbose: u8) {
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

    fn invalid_state() -> HyperVError {
        HyperVError::InvalidState {
            target: None,
            state: VmState::Off,
            operation: "pause".to_string(),
            raw: None,
        }
    }

    fn job_failed() -> HyperVError {
        HyperVError::JobFailed {
            code: 32770,
            message: "not supported".to_string(),
            reason: hypervctl_core::JobFailReason::Other,
            raw: None,
        }
    }

    #[test]
    fn every_classification_has_the_exit_code_of_the_table() {
        let table: Vec<(HyperVError, u8)> = vec![
            (not_found(), 3),
            (ambiguous(), 3),
            (HyperVError::PermissionDenied { raw: None }, 4),
            (timeout(), 5),
            (invalid_state(), 1),
            (job_failed(), 1),
            (HyperVError::backend(RawError::new("boom")), 1),
            (
                HyperVError::HyperVUnavailable {
                    reason: UnavailableReason::Unknown,
                    raw: None,
                },
                1,
            ),
        ];
        for (error, code) in table {
            let shown = error.to_string();
            assert_eq!(exit_code(&CliError::Backend(error)), code, "{shown}");
        }
    }

    #[test]
    fn the_usage_and_interruption_codes_are_the_documented_ones() {
        assert_eq!(exit_code(&CliError::usage("nope")), 2);
        assert_eq!(exit_code(&CliError::Aborted), 1);
        assert_eq!(exit_code(&CliError::Interrupted), 1);
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
        assert_eq!(hint(&ambiguous()), None);
        assert_eq!(hint(&timeout()), None);
        assert_eq!(hint(&invalid_state()), None);
        assert_eq!(hint(&job_failed()), None);
    }

    #[test]
    fn an_error_reports_message_candidates_and_hint() {
        let mut out = Vec::new();
        write_backend(&mut out, &ambiguous(), 0)
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
        write_backend(&mut quiet, &error, 0)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        let quiet = String::from_utf8_lossy(&quiet).into_owned();
        assert!(quiet.contains("hint: Run as Administrator"), "{quiet:?}");
        assert!(!quiet.contains("raw:"), "{quiet:?}");

        let mut loud = Vec::new();
        write_backend(&mut loud, &error, 1)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        let loud = String::from_utf8_lossy(&loud).into_owned();
        assert!(loud.contains("  raw: 0x80070005: denied"), "{loud:?}");
    }

    #[test]
    fn several_failures_are_reported_target_by_target_and_hint_once() {
        let failures = vec![
            (
                "web-02".to_string(),
                HyperVError::PermissionDenied { raw: None },
            ),
            (
                "db-01".to_string(),
                HyperVError::PermissionDenied { raw: None },
            ),
            ("nope".to_string(), not_found()),
        ];
        let mut out = Vec::new();
        write_failures(&mut out, &failures, 0)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.starts_with("error: web-02: permission denied\n"),
            "{text:?}"
        );
        assert!(
            text.contains("error: db-01: permission denied\n"),
            "{text:?}"
        );
        assert!(
            text.contains("error: nope: VM not found: 'nope'\n"),
            "{text:?}"
        );
        assert_eq!(
            text.matches("hint: Run as Administrator").count(),
            1,
            "the same classification must be hinted once: {text:?}"
        );
        assert_eq!(text.matches("hint:").count(), 1, "{text:?}");
    }

    #[test]
    fn a_usage_error_and_an_abort_print_their_own_line() {
        let mut out = Vec::new();
        write(&mut out, &CliError::usage("confirmation needed"), 0)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        assert_eq!(
            String::from_utf8_lossy(&out).into_owned(),
            "error: confirmation needed\n"
        );

        let mut out = Vec::new();
        write(&mut out, &CliError::Aborted, 0)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        assert_eq!(
            String::from_utf8_lossy(&out).into_owned(),
            "error: aborted\n"
        );

        let mut out = Vec::new();
        write(&mut out, &CliError::Interrupted, 0)
            .unwrap_or_else(|error| panic!("writing to a vector cannot fail: {error}"));
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.starts_with("error: interrupted;"), "{text:?}");
        assert!(text.contains("Hyper-V"), "{text:?}");
    }
}
