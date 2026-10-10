//! The verbs of `vm` (spec 2.3).

use hypervctl_core::{OpSuccess, Operations, VmOp};

use crate::cli::args::{VmCommand, WaitArgs};
use crate::cli::commands::{finish, CommandOutput, Context};
use crate::cli::confirm::{confirm, ConfirmTarget};
use crate::cli::errors::CliError;
use crate::cli::output::operations::{report, settled_word};
use crate::cli::output::views::{VmDetailView, VmListRow};
use crate::cli::output::{render_detail, render_list};
use crate::cli::progress::{waiting_label, ProgressReporter};

pub async fn run(
    command: &VmCommand,
    ops: &Operations,
    context: &Context<'_>,
) -> Result<CommandOutput, CliError> {
    match command {
        VmCommand::List => {
            let vms = ops.handle().list_vms().await?;
            Ok(CommandOutput::stdout(render_list(
                &VmListRow::from_list(vms),
                context.render,
            )?))
        }
        VmCommand::Get { vm } => {
            let id = ops.resolve_vm(vm).await?;
            let detail = ops.handle().get_vm(id).await?;
            Ok(CommandOutput::stdout(render_detail(
                &VmDetailView::from_detail(detail),
                context.render,
            )?))
        }
        VmCommand::Start { vm, wait } => operate(ops, context, VmOp::Start, vm, wait).await,
        VmCommand::Stop { vm, force, wait } => {
            let op = if *force { VmOp::StopForce } else { VmOp::Stop };
            operate(ops, context, op, vm, wait).await
        }
        VmCommand::Restart { vm, force, wait } => {
            let op = if *force {
                VmOp::RestartForce
            } else {
                VmOp::Restart
            };
            operate(ops, context, op, vm, wait).await
        }
        VmCommand::Save { vm, wait } => operate(ops, context, VmOp::Save, vm, wait).await,
        VmCommand::Pause { vm, wait } => operate(ops, context, VmOp::Pause, vm, wait).await,
        VmCommand::Resume { vm, wait } => operate(ops, context, VmOp::Resume, vm, wait).await,
    }
}

/// Runs one power verb over every target, in order, and never stops at the
/// first failure (spec 2.6).
async fn operate(
    ops: &Operations,
    context: &Context<'_>,
    op: VmOp,
    targets: &[String],
    wait: &WaitArgs,
) -> Result<CommandOutput, CliError> {
    if matches!(op, VmOp::StopForce | VmOp::RestartForce) {
        confirm_power(ops, context, op, targets).await?;
    }
    let label = waiting_label(op);
    // `--no-wait` has nothing to wait for, so it has nothing to report either
    // (design 4.5).
    let mut reporter = wait.waits().then(ProgressReporter::new);
    let outcomes = ops
        .run_vm(op, targets, wait.policy(), &mut |progress| {
            if let Some(reporter) = reporter.as_mut() {
                reporter.report(&progress, label);
            }
        })
        .await;
    if let Some(mut reporter) = reporter {
        reporter.clear();
    }
    let found = report(
        outcomes,
        |success| describe(success, op),
        context.global.verbose,
    );
    finish(found, context)
}

/// Asks once, listing every target by name and by id (design 4.6).
async fn confirm_power(
    ops: &Operations,
    context: &Context<'_>,
    op: VmOp,
    targets: &[String],
) -> Result<(), CliError> {
    let mut listed = Vec::with_capacity(targets.len());
    for spec in targets {
        let summary = ops.vm_summary(spec).await?;
        listed.push(ConfirmTarget {
            name: summary.name,
            id: summary.id.to_string(),
        });
    }
    let question = match op {
        VmOp::StopForce => "Force-stop these virtual machines?",
        VmOp::RestartForce => "Force-restart these virtual machines?",
        _ => "Run this operation on these virtual machines?",
    };
    confirm(question, &listed, context.global.yes, context.stdin_is_tty)
}

/// The line a success gets on the terminal (design 4.4, spec 2.9).
fn describe(success: &OpSuccess, op: VmOp) -> String {
    match success {
        OpSuccess::Already { state } => format!("already {}", settled_word(*state)),
        OpSuccess::Submitted { .. } => "submitted".to_string(),
        OpSuccess::Completed { .. } => past_tense(op).to_string(),
    }
}

pub(crate) fn past_tense(op: VmOp) -> &'static str {
    match op {
        VmOp::Start => "started",
        VmOp::Stop | VmOp::StopForce => "stopped",
        VmOp::Restart | VmOp::RestartForce => "restarted",
        VmOp::Save => "saved",
        VmOp::Pause => "paused",
        VmOp::Resume => "resumed",
    }
}
