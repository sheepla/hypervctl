//! The read-only verbs of `vm` (spec 2.3).

use hypervctl_core::{HyperVError, Operations};

use crate::cli::args::VmCommand;
use crate::cli::output::views::{VmDetailView, VmListRow};
use crate::cli::output::{render_detail, render_list, RenderOptions};

pub async fn run(
    command: &VmCommand,
    ops: &Operations,
    opts: &RenderOptions,
) -> Result<String, HyperVError> {
    match command {
        VmCommand::List => {
            let vms = ops.handle().list_vms().await?;
            render_list(&VmListRow::from_list(vms), opts)
        }
        VmCommand::Get { vm } => {
            let id = ops.resolve_vm(vm).await?;
            let detail = ops.handle().get_vm(id).await?;
            render_detail(&VmDetailView::from_detail(detail), opts)
        }
    }
}
