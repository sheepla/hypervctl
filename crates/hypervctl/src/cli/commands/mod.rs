//! Runs one parsed command against the operation layer and renders the result
//! (design 4.1).

pub mod checkpoint;
pub mod switch;
pub mod vhd;
pub mod vm;

use hypervctl_core::{HyperVError, Operations};

use crate::cli::args::Resource;

use super::output::RenderOptions;

pub async fn run(
    resource: &Resource,
    ops: &Operations,
    opts: &RenderOptions,
) -> Result<String, HyperVError> {
    match resource {
        Resource::Vm { command } => vm::run(command, ops, opts).await,
        Resource::Checkpoint { command } => checkpoint::run(command, ops, opts).await,
        Resource::Switch { command } => switch::run(command, ops, opts).await,
        Resource::Vhd { command } => vhd::run(command, ops, opts).await,
    }
}
