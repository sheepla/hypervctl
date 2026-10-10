//! The verbs of `checkpoint` (spec 2.4).

use hypervctl_core::{OpSuccess, Operations};

use crate::cli::args::CheckpointCommand;
use crate::cli::commands::{finish, CommandOutput, Context};
use crate::cli::confirm::{confirm, ConfirmTarget};
use crate::cli::errors::CliError;
use crate::cli::output::operations::report;
use crate::cli::output::render_list;
use crate::cli::output::views::CheckpointListRow;
use crate::cli::progress::ProgressReporter;

pub async fn run(
    command: &CheckpointCommand,
    ops: &Operations,
    context: &Context<'_>,
) -> Result<CommandOutput, CliError> {
    match command {
        CheckpointCommand::List { vm } => {
            let id = ops.resolve_vm(vm).await?;
            let checkpoints = ops.handle().list_checkpoints(id).await?;
            Ok(CommandOutput::stdout(render_list(
                &CheckpointListRow::from_list(checkpoints),
                context.render,
            )?))
        }
        CheckpointCommand::Create { vm, name, wait } => {
            // What the backend reports carries no identifier, so none is shown
            // (design 4.4).
            let mut reporter = wait.waits().then(ProgressReporter::new);
            let outcome = ops
                .create_checkpoint(vm, name.as_deref(), wait.policy(), &mut |progress| {
                    if let Some(reporter) = reporter.as_mut() {
                        reporter.report(&progress, "creating checkpoint");
                    }
                })
                .await;
            clear(&mut reporter);
            let found = report(
                vec![outcome],
                |success| describe(success, "checkpoint created"),
                context.global.verbose,
            );
            finish(found, context)
        }
        CheckpointCommand::Restore {
            vm,
            checkpoint,
            wait,
        } => {
            confirm_checkpoint(ops, context, "Restore these checkpoints?", vm, checkpoint).await?;
            let mut reporter = wait.waits().then(ProgressReporter::new);
            let outcome = ops
                .restore_checkpoint(vm, checkpoint, wait.policy(), &mut |progress| {
                    if let Some(reporter) = reporter.as_mut() {
                        reporter.report(&progress, "restoring checkpoint");
                    }
                })
                .await;
            clear(&mut reporter);
            let found = report(
                vec![outcome],
                |success| describe(success, "restored"),
                context.global.verbose,
            );
            finish(found, context)
        }
        CheckpointCommand::Remove {
            vm,
            checkpoint,
            wait,
        } => {
            confirm_checkpoint(ops, context, "Remove these checkpoints?", vm, checkpoint).await?;
            let mut reporter = wait.waits().then(ProgressReporter::new);
            let outcome = ops
                .remove_checkpoint(vm, checkpoint, wait.policy(), &mut |progress| {
                    if let Some(reporter) = reporter.as_mut() {
                        reporter.report(&progress, "removing checkpoint");
                    }
                })
                .await;
            clear(&mut reporter);
            let found = report(
                vec![outcome],
                |success| describe(success, "removed"),
                context.global.verbose,
            );
            finish(found, context)
        }
    }
}

/// Asks once about the one checkpoint the command acts on (design 4.6).
async fn confirm_checkpoint(
    ops: &Operations,
    context: &Context<'_>,
    question: &str,
    vm: &str,
    checkpoint: &str,
) -> Result<(), CliError> {
    let vm_id = ops.resolve_vm(vm).await?;
    let entry = ops.checkpoint_entry(vm_id, checkpoint).await?;
    let listed = vec![ConfirmTarget {
        name: entry.name,
        id: entry.id.to_string(),
    }];
    confirm(question, &listed, context.global.yes, context.stdin_is_tty)
}

fn clear(reporter: &mut Option<ProgressReporter<std::io::Stderr>>) {
    if let Some(reporter) = reporter.as_mut() {
        reporter.clear();
    }
}

/// The line a success gets on the terminal (design 4.4).
fn describe(success: &OpSuccess, done: &'static str) -> String {
    match success {
        OpSuccess::Already { .. } => format!("already {done}"),
        OpSuccess::Submitted { .. } => "submitted".to_string(),
        OpSuccess::Completed { .. } => done.to_string(),
    }
}
