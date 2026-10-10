//! Runs one parsed command against the operation layer and renders the result
//! (design 4.1).

pub mod checkpoint;
pub mod switch;
pub mod tui;
pub mod vhd;
pub mod vm;

use std::io::IsTerminal;

use hypervctl_core::Operations;

use crate::cli::args::{Format, GlobalArgs, Resource};
use crate::cli::errors::CliError;
use crate::cli::output::operations::{render_rows, OpReport};
use crate::cli::output::RenderOptions;

/// What a command leaves behind: data for stdout, messages for stderr and the
/// exit code (design 4.2).
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: u8,
}

impl CommandOutput {
    /// A read-only command: data on stdout, nothing else (design 4.2).
    pub fn stdout(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            code: 0,
        }
    }
}

/// Everything a command needs that is not its own arguments (design 4.2).
pub struct Context<'a> {
    pub global: &'a GlobalArgs,
    pub render: &'a RenderOptions,
    /// Whether a confirmation prompt may be asked at all (spec 2.8).
    pub stdin_is_tty: bool,
}

impl<'a> Context<'a> {
    pub fn new(global: &'a GlobalArgs, render: &'a RenderOptions) -> Self {
        Self {
            global,
            render,
            stdin_is_tty: std::io::stdin().is_terminal(),
        }
    }
}

pub async fn run(
    resource: &Resource,
    ops: &Operations,
    context: &Context<'_>,
) -> Result<CommandOutput, CliError> {
    match resource {
        Resource::Vm { command } => vm::run(command, ops, context).await,
        Resource::Checkpoint { command } => checkpoint::run(command, ops, context).await,
        Resource::Switch { command } => switch::run(command, ops, context).await,
        Resource::Vhd { command } => vhd::run(command, ops, context).await,
        Resource::Tui { refresh } => tui::run(*refresh, ops, context).await,
    }
}

/// Turns the outcomes of an operation into the output of its command
/// (design 4.4 and 4.7): rows on stdout for `-o json` and `-o csv`, one line per
/// target on stderr for a terminal, and the exit code they add up to.
pub(crate) fn finish(found: OpReport, context: &Context<'_>) -> Result<CommandOutput, CliError> {
    // Every target failed the same way before any of them ran: one error, one
    // exit code, no per-target noise (design 4.7).
    if let Some(fatal) = found.fatal {
        return Err(CliError::Backend(fatal));
    }
    let stdout = render_rows(&found.rows, context.render)?;
    let mut stderr = String::new();
    if context.render.format == Format::Table {
        for line in &found.success_lines {
            stderr.push_str(line);
            stderr.push('\n');
        }
    }
    stderr.push_str(&found.failure_text);
    Ok(CommandOutput {
        stdout,
        stderr,
        code: found.code,
    })
}
