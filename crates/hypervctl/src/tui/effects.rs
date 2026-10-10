//! Running the effects away from the main loop and returning their results as
//! actions (design 5.2 and 5.4).

use hypervctl_core::BackendHandle;
use tokio::sync::mpsc;

use crate::tui::store::{Action, Effect};

/// Spawns `effect` on the runtime; its answer comes back through `actions`.
pub fn spawn(effect: Effect, handle: &BackendHandle, actions: &mpsc::UnboundedSender<Action>) {
    match effect {
        Effect::FetchVmList => {
            let handle = handle.clone();
            let actions = actions.clone();
            tokio::spawn(async move {
                let result = handle.list_vms().await;
                let _ = actions.send(Action::VmListLoaded(Box::new(result)));
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
        // The loop breaks on this one; nothing to run.
        Effect::Quit => {}
    }
}
