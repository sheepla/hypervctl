//! The read-only verbs of `vhd` (spec 2.5).

use hypervctl_core::Operations;

use crate::cli::args::VhdCommand;
use crate::cli::commands::{CommandOutput, Context};
use crate::cli::errors::CliError;
use crate::cli::output::views::{VhdDetailView, VhdListRow};
use crate::cli::output::{render_detail, render_list};

pub async fn run(
    command: &VhdCommand,
    ops: &Operations,
    context: &Context<'_>,
) -> Result<CommandOutput, CliError> {
    match command {
        VhdCommand::List { vm } => {
            let id = match vm {
                Some(spec) => Some(ops.resolve_vm(spec).await?),
                None => None,
            };
            let vhds = ops.handle().list_vhds(id).await?;
            Ok(CommandOutput::stdout(render_list(
                &VhdListRow::from_list(vhds),
                context.render,
            )?))
        }
        VhdCommand::Get { path } => {
            let info = ops.handle().get_vhd(path).await?;
            Ok(CommandOutput::stdout(render_detail(
                &VhdDetailView::from_info(info),
                context.render,
            )?))
        }
    }
}
