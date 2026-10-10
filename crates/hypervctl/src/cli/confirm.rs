//! The confirmation prompt (design 4.6): one `y`-or-nothing question per
//! command, never one per target.

use std::io::{BufRead, Write};

use crate::cli::errors::CliError;

/// One line of the prompt: what will be acted on, by name and by id
/// (design 4.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmTarget {
    pub name: String,
    pub id: String,
}

/// Asks on the real terminal, unless `-y` said not to (spec 2.8).
pub fn confirm(
    question: &str,
    targets: &[ConfirmTarget],
    yes: bool,
    stdin_is_tty: bool,
) -> Result<(), CliError> {
    if yes {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    confirm_in(
        question,
        targets,
        stdin_is_tty,
        &mut std::io::stdin().lock(),
        &mut stderr,
    )
}

/// The prompt itself, on any pair of streams (design 4.6).
///
/// Anything but `y` or `yes`, in any letter case, declines; so does an empty
/// answer, which is what the `N` of `[y/N]` stands for.
pub fn confirm_in<R, W>(
    question: &str,
    targets: &[ConfirmTarget],
    stdin_is_tty: bool,
    input: &mut R,
    out: &mut W,
) -> Result<(), CliError>
where
    R: BufRead,
    W: Write,
{
    if !stdin_is_tty {
        return Err(CliError::usage(
            "confirmation needed but stdin is not a terminal; pass -y to proceed",
        ));
    }
    // The prompt is a courtesy on stderr: a sink that will not take it must
    // not turn into an error of its own.
    let _ = writeln!(out, "{question}");
    for target in targets {
        let _ = writeln!(out, "  {}  {}", target.name, target.id);
    }
    let _ = write!(out, "[y/N] ");
    let _ = out.flush();
    let mut answer = String::new();
    if input.read_line(&mut answer).is_err() {
        return Err(CliError::Aborted);
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => Err(CliError::Aborted),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn targets() -> Vec<ConfirmTarget> {
        vec![
            ConfirmTarget {
                name: "web-01".to_string(),
                id: "00000001-0000-0000-0000-000000000000".to_string(),
            },
            ConfirmTarget {
                name: "db-01".to_string(),
                id: "00000003-0000-0000-0000-000000000000".to_string(),
            },
        ]
    }

    fn ask(answer: &str) -> Result<(), CliError> {
        let mut out = Vec::new();
        let result = confirm_in(
            "Force-stop these virtual machines?",
            &targets(),
            true,
            &mut Cursor::new(answer.to_string()),
            &mut out,
        );
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.starts_with("Force-stop these virtual machines?\n"),
            "{text:?}"
        );
        assert!(text.contains("  web-01  00000001-"), "{text:?}");
        assert!(text.contains("  db-01  00000003-"), "{text:?}");
        assert!(text.ends_with("[y/N] "), "{text:?}");
        result
    }

    #[test]
    fn only_y_and_yes_go_through() {
        assert!(ask("y\n").is_ok());
        assert!(ask("yes\n").is_ok());
        assert!(ask("Y\n").is_ok());
        assert!(ask("YES\n").is_ok());
    }

    #[test]
    fn anything_else_declines() {
        assert_eq!(ask("n\n"), Err(CliError::Aborted));
        assert_eq!(ask("\n"), Err(CliError::Aborted));
        assert_eq!(ask("no\n"), Err(CliError::Aborted));
        assert_eq!(ask("sure\n"), Err(CliError::Aborted));
    }

    #[test]
    fn a_pipe_is_refused_instead_of_being_asked() {
        let mut out = Vec::new();
        let error = confirm_in(
            "Remove these checkpoints?",
            &targets(),
            false,
            &mut Cursor::new("y\n"),
            &mut out,
        );
        match error {
            Err(CliError::Usage(message)) => {
                assert!(message.contains("-y"), "{message:?}");
                assert!(message.contains("stdin"), "{message:?}");
            }
            other => panic!("a pipe must be refused, got {other:?}"),
        }
        assert!(out.is_empty(), "nothing may be asked of a pipe: {out:?}");
    }

    #[test]
    fn yes_short_circuits_the_prompt() {
        // `confirm` returns before looking at the terminal when `-y` is given.
        assert!(
            crate::cli::confirm::confirm("Remove these checkpoints?", &targets(), true, false)
                .is_ok()
        );
    }
}
