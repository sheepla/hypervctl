//! Running the effects away from the main loop and returning their results as
//! actions (design 5.2 and 5.4).

use std::time::Instant;

use hypervctl_core::{BackendHandle, OpSuccess, Operations, WaitPolicy};
use tokio::sync::mpsc;

use crate::cli::commands::vm::past_tense;
use crate::cli::output::operations::settled_word;
use crate::tui::store::{Action, Effect, OpFailure, OpRequest};

/// Spawns `effect` on the runtime; its answer comes back through `actions`.
pub fn spawn(
    effect: Effect,
    ops: &Operations,
    handle: &BackendHandle,
    actions: &mpsc::UnboundedSender<Action>,
) {
    match effect {
        Effect::FetchVmList => {
            let handle = handle.clone();
            let actions = actions.clone();
            tokio::spawn(async move {
                let result = handle.list_vms().await;
                // The time the list lands carries the samples the Monitor
                // records (design 5.12).
                let _ = actions.send(Action::VmListLoaded {
                    result: Box::new(result),
                    at: Instant::now(),
                });
            });
        }
        Effect::FetchVmDetail(id) => {
            let handle = handle.clone();
            let actions = actions.clone();
            tokio::spawn(async move {
                let result = handle.get_vm(id).await;
                let _ = actions.send(Action::VmDetailLoaded {
                    id,
                    result: Box::new(result),
                });
            });
        }
        Effect::FetchCheckpoints(vm) => {
            let handle = handle.clone();
            let actions = actions.clone();
            tokio::spawn(async move {
                let result = handle.list_checkpoints(vm).await;
                let _ = actions.send(Action::CheckpointsLoaded {
                    vm,
                    result: Box::new(result),
                });
            });
        }
        Effect::RunOperation(request) => {
            let ops = ops.clone();
            let actions = actions.clone();
            tokio::spawn(async move {
                let vm = request.vm();
                // The progress callback of the operation layer is not `Sync`,
                // so the future is not `Send`; a blocking task may run it on the
                // runtime anyway.
                let outcome = tokio::task::spawn_blocking(move || {
                    tokio::runtime::Handle::current().block_on(run(&ops, &request))
                })
                .await;
                let result = match outcome {
                    Ok(Ok(message)) => Ok(message),
                    Ok(Err((message, error))) => Err(OpFailure { message, error }),
                    Err(join) => Err(OpFailure {
                        message: format!("the operation task failed: {join}"),
                        error: hypervctl_core::stopped_error(),
                    }),
                };
                let _ = actions.send(Action::OpFinished {
                    vm,
                    result,
                    at: Instant::now(),
                });
            });
        }
        // The loop breaks on this one; nothing to run.
        Effect::Quit => {}
    }
}

/// One operation of the menu, waiting for its goal (spec 3.5). The message is
/// built here so that the reducer stays free of wording.
async fn run(
    ops: &Operations,
    request: &OpRequest,
) -> Result<String, (String, hypervctl_core::HyperVError)> {
    let wait = WaitPolicy::Wait(Some(WaitPolicy::DEFAULT_TIMEOUT));
    let mut quiet = |_: hypervctl_core::Progress| {};
    let outcome = match request {
        OpRequest::Vm { vm_name, op, .. } => {
            let outcomes = ops
                .run_vm(op.op(), std::slice::from_ref(vm_name), wait, &mut quiet)
                .await;
            outcomes.into_iter().next()
        }
        OpRequest::CreateCheckpoint { vm_name, .. } => {
            Some(ops.create_checkpoint(vm_name, None, wait, &mut quiet).await)
        }
        OpRequest::RestoreCheckpoint {
            vm_name,
            checkpoint_name,
            ..
        } => Some(
            ops.restore_checkpoint(vm_name, checkpoint_name, wait, &mut quiet)
                .await,
        ),
        OpRequest::RemoveCheckpoint {
            vm_name,
            checkpoint_name,
            ..
        } => Some(
            ops.remove_checkpoint(vm_name, checkpoint_name, wait, &mut quiet)
                .await,
        ),
    };
    let Some(outcome) = outcome else {
        return Err((
            "no target was reported for this operation".to_string(),
            hypervctl_core::stopped_error(),
        ));
    };
    match outcome.result {
        Ok(success) => Ok(describe(request, &success)),
        Err(error) => Err((format!("{}: {error}", outcome.target), error)),
    }
}

/// The line a success gets on the status bar (spec 3.5).
fn describe(request: &OpRequest, success: &OpSuccess) -> String {
    let phrase = match request {
        OpRequest::Vm { op, .. } => match success {
            OpSuccess::Already { state } => {
                return format!(
                    "{}: already {}",
                    request_name(request),
                    settled_word(*state)
                );
            }
            OpSuccess::Submitted { .. } => "submitted".to_string(),
            OpSuccess::Completed { .. } => past_tense(op.op()).to_string(),
        },
        OpRequest::CreateCheckpoint { .. } => match success {
            OpSuccess::Already { state } => {
                return format!(
                    "{}: already {}",
                    request_name(request),
                    settled_word(*state)
                );
            }
            OpSuccess::Submitted { .. } => "submitted".to_string(),
            OpSuccess::Completed { .. } => "checkpoint created".to_string(),
        },
        OpRequest::RestoreCheckpoint { .. } => match success {
            OpSuccess::Already { state } => {
                return format!(
                    "{}: already {}",
                    request_name(request),
                    settled_word(*state)
                );
            }
            OpSuccess::Submitted { .. } => "submitted".to_string(),
            OpSuccess::Completed { .. } => "checkpoint restored".to_string(),
        },
        OpRequest::RemoveCheckpoint { .. } => match success {
            OpSuccess::Already { .. } => "already gone".to_string(),
            OpSuccess::Submitted { .. } => "submitted".to_string(),
            OpSuccess::Completed { .. } => "checkpoint removed".to_string(),
        },
    };
    format!("{}: {phrase}", request_name(request))
}

fn request_name(request: &OpRequest) -> &str {
    match request {
        OpRequest::Vm { vm_name, .. }
        | OpRequest::CreateCheckpoint { vm_name, .. }
        | OpRequest::RestoreCheckpoint { vm_name, .. }
        | OpRequest::RemoveCheckpoint { vm_name, .. } => vm_name,
    }
}
