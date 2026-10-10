//! The read-only verbs of `vhd` (spec 2.5).

use hypervctl_core::{HyperVError, Operations};

use crate::cli::args::VhdCommand;
use crate::cli::output::views::{VhdDetailView, VhdListRow};
use crate::cli::output::{render_detail, render_list, RenderOptions};

pub async fn run(
    command: &VhdCommand,
    ops: &Operations,
    opts: &RenderOptions,
) -> Result<String, HyperVError> {
    match command {
        VhdCommand::List { vm } => {
            let id = match vm {
                Some(spec) => Some(ops.resolve_vm(spec).await?),
                None => None,
            };
            let vhds = ops.handle().list_vhds(id).await?;
            render_list(&VhdListRow::from_list(vhds), opts)
        }
        VhdCommand::Get { path } => {
            let info = ops.handle().get_vhd(path).await?;
            render_detail(&VhdDetailView::from_info(info), opts)
        }
    }
}
