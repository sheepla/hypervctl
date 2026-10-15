//! The result rows of the operation commands (design 4.4) and the exit code
//! their outcomes add up to (design 4.7).

use serde::Serialize;

use hypervctl_core::{HyperVError, OpId, OpOutcome, OpSuccess, VmState};

use super::{csv, json, to_value, RenderOptions};
use crate::cli::args::Format;
use crate::cli::errors::{backend_exit_code, write_backend, write_failures};

/// What became of one target (design 4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpRow {
    /// The identifier of what was acted on, in full; `null` when the target
    /// never resolved (design 4.4).
    pub id: Option<String>,
    /// The target exactly as the user typed it.
    pub name: String,
    pub status: OpStatus,
    /// Present only when the operation failed (design 4.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OpError>,
}

/// The status half of a row (design 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpStatus {
    /// The goal was reached.
    Succeeded,
    /// The goal was already met, so nothing was requested (spec 2.9).
    Already,
    /// Accepted, not waited for (`--no-wait`).
    Submitted,
    /// The target failed; `error` says how.
    Failed,
}

/// The error of a failed row (design 4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpError {
    /// The classification, as a stable machine-readable name (design 4.4).
    pub kind: String,
    pub message: String,
}

/// Everything an operation leaves behind: what to print, and how to exit
/// (design 4.4 and 4.7).
pub struct OpReport {
    /// One row per target, for `-o json` and `-o csv`.
    pub rows: Vec<OpRow>,
    /// One line per target, for a human watching the terminal (design 4.4).
    pub success_lines: Vec<String>,
    /// `error:` lines and hints: stderr carries them in every format
    /// (spec 2.10).
    pub failure_text: String,
    /// The exit code (spec 2.11); always `0` when `fatal` is set, because then
    /// the command fails as a whole instead.
    pub code: u8,
    /// When every target failed the same way before any of them could run, the
    /// one error that stands for the whole command (design 4.7).
    pub fatal: Option<HyperVError>,
}

/// Turns the outcomes of one command into rows, lines and an exit code.
///
/// `describe` words a success for a human (`stopped`, `already running`); the
/// machine-readable rows take their status from the outcome itself.
pub fn report<F>(outcomes: Vec<OpOutcome>, describe: F, verbose: u8) -> OpReport
where
    F: Fn(&OpSuccess) -> String,
{
    let total = outcomes.len();
    let mut rows = Vec::with_capacity(total);
    let mut success_lines = Vec::new();
    let mut failures: Vec<(String, HyperVError)> = Vec::new();
    let mut failed = 0usize;

    for outcome in outcomes {
        let id = outcome.id.map(id_text);
        match outcome.result {
            Ok(success) => {
                let status = match success {
                    OpSuccess::Already { .. } => OpStatus::Already,
                    OpSuccess::Submitted { .. } => OpStatus::Submitted,
                    OpSuccess::Completed { .. } => OpStatus::Succeeded,
                };
                success_lines.push(format!("{}: {}", outcome.target, describe(&success)));
                rows.push(OpRow {
                    id,
                    name: outcome.target,
                    status,
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                let message = error.to_string();
                rows.push(OpRow {
                    id,
                    name: outcome.target.clone(),
                    status: OpStatus::Failed,
                    error: Some(OpError {
                        kind: error.kind().to_string(),
                        message,
                    }),
                });
                failures.push((outcome.target, error));
            }
        }
    }

    // One target failed, so the target is already named by the command line:
    // report the error the way every other single-command failure is reported.
    // Several targets get one line each (design 4.7).
    let mut text = Vec::new();
    if total == 1 {
        if let Some((_, error)) = failures.first() {
            let _ = write_backend(&mut text, error, verbose);
        }
    } else {
        let _ = write_failures(&mut text, &failures, verbose);
    }
    let failure_text = String::from_utf8_lossy(&text).into_owned();

    // One failure that stands for everybody: the backend could not be reached
    // at all (design 4.7). Everything else that fails at least once is a 1 when
    // there were several targets, and its own classification when there was one.
    let whole = failed > 0
        && failed == total
        && failures.iter().all(|(_, error)| {
            matches!(
                error,
                HyperVError::PermissionDenied { .. } | HyperVError::HyperVUnavailable { .. }
            )
        })
        && failures
            .windows(2)
            .all(|pair| std::mem::discriminant(&pair[0].1) == std::mem::discriminant(&pair[1].1));

    let code = if failed == 0 {
        0
    } else if whole {
        failures
            .first()
            .map(|(_, error)| backend_exit_code(error))
            .unwrap_or(1)
    } else if failed == 1 && total == 1 {
        failures
            .first()
            .map(|(_, error)| backend_exit_code(error))
            .unwrap_or(1)
    } else {
        1
    };

    let fatal = if whole {
        let mut taken = failures.into_iter();
        taken.next().map(|(_, error)| error)
    } else {
        None
    };

    OpReport {
        rows,
        success_lines,
        failure_text,
        code,
        fatal,
    }
}

/// Renders the rows for stdout: nothing at all for a table, one record per
/// target for JSON and CSV (design 4.4).
pub fn render_rows(rows: &[OpRow], opts: &RenderOptions) -> Result<String, HyperVError> {
    match opts.format {
        Format::Table => Ok(String::new()),
        Format::Json => json::render(rows),
        Format::Csv => Ok(csv::render(&to_value(rows)?, opts.no_header)),
    }
}

/// What a settled VM is called on a line that says it was already there
/// (spec 2.9).
pub fn settled_word(state: VmState) -> &'static str {
    use VmState::*;
    match state {
        Running => "running",
        Off => "stopped",
        Saved => "saved",
        Paused => "paused",
        // Unreachable: `already` is only reported for a goal state.
        _ => "settled",
    }
}

fn id_text(id: OpId) -> String {
    match id {
        OpId::Vm(id) => id.to_string(),
        OpId::Checkpoint(id) => id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hypervctl_core::{CheckpointId, VmId};
    use uuid::Uuid;

    use super::*;

    fn vm_outcome(target: &str, result: Result<OpSuccess, HyperVError>) -> OpOutcome {
        OpOutcome {
            target: target.to_string(),
            id: Some(OpId::Vm(VmId::new(Uuid::from_u128(0x1 << 96)))),
            created: None,
            result,
        }
    }

    fn completed() -> OpSuccess {
        OpSuccess::Completed {
            state: VmState::Off,
            elapsed: Duration::from_secs(2),
        }
    }

    fn describe(success: &OpSuccess) -> String {
        match success {
            OpSuccess::Already { state } => format!("already {}", settled_word(*state)),
            OpSuccess::Submitted { .. } => "submitted".to_string(),
            OpSuccess::Completed { .. } => "stopped".to_string(),
        }
    }

    #[test]
    fn successes_become_rows_and_lines() {
        let outcomes = vec![
            vm_outcome("web-01", Ok(completed())),
            vm_outcome(
                "web-02",
                Ok(OpSuccess::Already {
                    state: VmState::Off,
                }),
            ),
        ];
        let found = report(outcomes, describe, 0);
        assert_eq!(found.code, 0);
        assert!(found.fatal.is_none());
        assert_eq!(
            found.success_lines,
            vec![
                "web-01: stopped".to_string(),
                "web-02: already stopped".to_string()
            ]
        );
        assert_eq!(found.rows[0].status, OpStatus::Succeeded);
        assert_eq!(
            found.rows[0].id.as_deref(),
            Some("00000001-0000-0000-0000-000000000000")
        );
        assert_eq!(found.rows[1].status, OpStatus::Already);
        assert!(found.rows.iter().all(|row| row.error.is_none()));
        assert!(found.failure_text.is_empty());
    }

    #[test]
    fn one_failure_among_several_targets_is_a_one() {
        let outcomes = vec![
            vm_outcome("web-01", Ok(completed())),
            OpOutcome {
                target: "nope".to_string(),
                id: None,
                created: None,
                result: Err(HyperVError::NotFound {
                    kind: hypervctl_core::TargetKind::Vm,
                    specified: "nope".to_string(),
                    raw: None,
                }),
            },
        ];
        let found = report(outcomes, describe, 0);
        assert_eq!(found.code, 1);
        assert!(found.fatal.is_none());
        assert!(
            found
                .failure_text
                .contains("error: nope: VM not found: 'nope'"),
            "{:?}",
            found.failure_text
        );
        assert_eq!(found.rows[1].status, OpStatus::Failed);
        assert_eq!(
            found.rows[1]
                .error
                .as_ref()
                .map(|error| error.kind.as_str()),
            Some("not_found")
        );
    }

    #[test]
    fn one_target_falls_back_to_its_own_classification() {
        let outcomes = vec![OpOutcome {
            target: "nope".to_string(),
            id: None,
            created: None,
            result: Err(HyperVError::NotFound {
                kind: hypervctl_core::TargetKind::Vm,
                specified: "nope".to_string(),
                raw: None,
            }),
        }];
        let found = report(outcomes, describe, 0);
        assert_eq!(found.code, 3);
        assert!(found.fatal.is_none());
        // The command line already names the one target, so the error is not
        // prefixed with it a second time.
        assert!(
            found
                .failure_text
                .starts_with("error: VM not found: 'nope'\n"),
            "{:?}",
            found.failure_text
        );
    }

    #[test]
    fn everybody_denied_is_one_error_for_the_whole_command() {
        let outcomes = vec![
            vm_outcome("web-01", Err(HyperVError::PermissionDenied { raw: None })),
            vm_outcome("web-02", Err(HyperVError::PermissionDenied { raw: None })),
        ];
        let found = report(outcomes, describe, 0);
        assert_eq!(found.code, 4);
        assert!(matches!(
            found.fatal,
            Some(HyperVError::PermissionDenied { .. })
        ));
    }

    #[test]
    fn everybody_missing_with_several_targets_is_still_a_one() {
        let not_found = || HyperVError::NotFound {
            kind: hypervctl_core::TargetKind::Vm,
            specified: "gone".to_string(),
            raw: None,
        };
        let outcomes = vec![
            OpOutcome {
                target: "gone".to_string(),
                id: None,
                created: None,
                result: Err(not_found()),
            },
            OpOutcome {
                target: "gone2".to_string(),
                id: None,
                created: None,
                result: Err(not_found()),
            },
        ];
        let found = report(outcomes, describe, 0);
        assert_eq!(found.code, 1);
        assert!(found.fatal.is_none());
    }

    #[test]
    fn rows_render_as_json_and_csv_and_not_as_a_table() {
        let rows = vec![
            OpRow {
                id: Some("00000001-0000-0000-0000-000000000000".to_string()),
                name: "web-01".to_string(),
                status: OpStatus::Already,
                error: None,
            },
            OpRow {
                id: None,
                name: "nope".to_string(),
                status: OpStatus::Failed,
                error: Some(OpError {
                    kind: "not_found".to_string(),
                    message: "VM not found: 'nope'".to_string(),
                }),
            },
        ];
        let opts = RenderOptions {
            format: Format::Json,
            no_header: false,
            no_trunc: true,
            color: false,
            max_width: None,
        };
        let json = render_rows(&rows, &opts).unwrap_or_else(|error| panic!("{error}"));
        let parsed: serde_json::Value =
            serde_json::from_str(&json).unwrap_or_else(|error| panic!("{error}: {json}"));
        assert_eq!(parsed[0]["name"], "web-01");
        assert_eq!(parsed[0]["status"], "already");
        assert_eq!(parsed[0]["error"], serde_json::Value::Null);
        assert_eq!(parsed[1]["status"], "failed");
        assert_eq!(parsed[1]["error"]["kind"], "not_found");

        let opts = RenderOptions {
            format: Format::Csv,
            ..opts
        };
        let csv = render_rows(&rows, &opts).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            csv.starts_with("id,name,status,error_kind,error_message\r\n"),
            "{csv:?}"
        );
        assert!(csv.contains("web-01,already"), "{csv:?}");
        assert!(
            csv.contains("nope,failed,not_found,VM not found: 'nope'"),
            "{csv:?}"
        );

        let opts = RenderOptions {
            format: Format::Table,
            ..opts
        };
        assert_eq!(
            render_rows(&rows, &opts).unwrap_or_else(|error| panic!("{error}")),
            ""
        );
    }

    #[test]
    fn a_checkpoint_row_keeps_its_checkpoint_identifier() {
        let outcome = OpOutcome {
            target: "nightly".to_string(),
            id: Some(OpId::Checkpoint(CheckpointId::new(Uuid::from_u128(
                0x101 << 96,
            )))),
            created: None,
            result: Ok(completed()),
        };
        let found = report(vec![outcome], |_| "restored".to_string(), 0);
        assert_eq!(
            found.rows[0].id.as_deref(),
            Some("00000101-0000-0000-0000-000000000000")
        );
    }

    #[test]
    fn the_word_of_a_settled_vm_matches_the_specification() {
        assert_eq!(settled_word(VmState::Running), "running");
        assert_eq!(settled_word(VmState::Off), "stopped");
        assert_eq!(settled_word(VmState::Saved), "saved");
        assert_eq!(settled_word(VmState::Paused), "paused");
    }
}
