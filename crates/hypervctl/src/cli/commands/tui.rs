//! The `tui` command: hands the terminal over to the interface (design 5.10).

use std::io::IsTerminal;
use std::time::Duration;

use hypervctl_core::Operations;

use crate::cli::commands::{CommandOutput, Context};
use crate::cli::errors::CliError;

pub async fn run(
    refresh: u64,
    ops: &Operations,
    context: &Context<'_>,
) -> Result<CommandOutput, CliError> {
    // The TUI draws on stdout and reads keys from stdin; either missing means
    // the interface cannot run at all (spec 3.1).
    if !context.stdin_is_tty || !std::io::stdout().is_terminal() {
        return Err(CliError::usage(
            "the tui needs an interactive terminal on stdin and stdout",
        ));
    }
    crate::tui::app::run(ops, Duration::from_secs(refresh)).await
}
