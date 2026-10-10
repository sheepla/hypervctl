//! The read-only verbs of `switch` (spec 2.5).

use hypervctl_core::Operations;

use crate::cli::args::{Format, SwitchCommand};
use crate::cli::commands::{CommandOutput, Context};
use crate::cli::errors::CliError;
use crate::cli::output::views::{SwitchDetailView, SwitchListRow};
use crate::cli::output::{render_detail, render_list};

pub async fn run(
    command: &SwitchCommand,
    ops: &Operations,
    context: &Context<'_>,
) -> Result<CommandOutput, CliError> {
    match command {
        SwitchCommand::List => {
            let switches = ops.handle().list_switches().await?;
            Ok(CommandOutput::stdout(render_list(
                &SwitchListRow::from_list(switches),
                context.render,
            )?))
        }
        SwitchCommand::Get { switch } => {
            let id = ops.resolve_switch(switch).await?;
            let detail = ops.handle().get_switch(id).await?;
            // Only `table` names the connected VMs: the other formats carry
            // the identifiers and pay no extra round trip (design 4.4).
            let names = if context.render.format == Format::Table {
                let vms = ops.handle().list_vms().await?;
                detail
                    .connected_vm_ids
                    .iter()
                    .map(|vm_id| {
                        vms.iter()
                            .find(|vm| vm.id == *vm_id)
                            .map_or_else(|| vm_id.to_string(), |vm| vm.name.clone())
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut view = SwitchDetailView::from_detail(detail);
            view.connected_vm_names = names;
            Ok(CommandOutput::stdout(render_detail(&view, context.render)?))
        }
    }
}
