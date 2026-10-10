//! The read-only verbs of `checkpoint` (spec 2.4).

use hypervctl_core::{HyperVError, Operations};

use crate::cli::args::CheckpointCommand;
use crate::cli::output::views::CheckpointListRow;
use crate::cli::output::{render_list, RenderOptions};

pub async fn run(
    command: &CheckpointCommand,
    ops: &Operations,
    opts: &RenderOptions,
) -> Result<String, HyperVError> {
    match command {
        CheckpointCommand::List { vm } => {
            let id = ops.resolve_vm(vm).await?;
            let checkpoints = ops.handle().list_checkpoints(id).await?;
            render_list(&CheckpointListRow::from_list(checkpoints), opts)
        }
    }
}
