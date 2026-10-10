//! The state of the VM list pane: the sorted items, the selection kept by id
//! (design 5.3 and 5.6), and the flags that keep polling from piling up
//! (design 5.5).

use hypervctl_core::{VmId, VmSummary};

/// Everything the list pane knows (design 5.3).
#[derive(Debug, Default)]
pub struct VmListState {
    /// Sorted by name, then id (spec 3.2).
    pub items: Vec<VmSummary>,
    /// The selected VM, by id so a refresh never moves the selection to
    /// another VM (spec 3.6).
    pub selected: Option<VmId>,
    /// Where the selection was, so that a vanished VM falls back to the
    /// nearest row (design 5.6).
    pub last_index: usize,
    /// A fetch is running; a tick must not start a second one (design 5.5).
    pub refresh_in_flight: bool,
    /// Something changed while a fetch was running: fetch once more after it.
    pub refresh_again: bool,
    /// The last refresh failed; the list on screen is out of date (design 5.5).
    pub stale: Option<String>,
}

impl VmListState {
    /// The position of the selected VM, when it is still in the list.
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.items.iter().position(|vm| vm.id == id)
    }

    /// Installs a fresh list, keeping the selection by id (design 5.6).
    ///
    /// Returns the name of the VM that was selected and is gone, so the caller
    /// can tell the user (acceptance 9).
    pub fn apply(&mut self, items: Vec<VmSummary>) -> Option<String> {
        let vanished = match self.selected {
            Some(id) if items.iter().all(|vm| vm.id != id) => self.name_of(id),
            _ => None,
        };
        self.items = items;
        self.stale = None;
        if self.selected.is_some() {
            if let Some(index) = self.selected_index() {
                self.last_index = index;
                return None;
            }
        }
        if self.items.is_empty() {
            self.selected = None;
            self.last_index = 0;
            return vanished;
        }
        let index = self.last_index.min(self.items.len() - 1);
        self.selected = self.items.get(index).map(|vm| vm.id);
        self.last_index = index;
        vanished
    }

    fn name_of(&self, id: VmId) -> Option<String> {
        self.items
            .iter()
            .find(|vm| vm.id == id)
            .map(|vm| vm.name.clone())
    }

    /// Moves the selection by `delta`, clamped at both ends.
    pub fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let current = self.selected_index().unwrap_or(self.last_index);
        let last = self.items.len() - 1;
        let target = current as isize + delta;
        let index = target.clamp(0, last as isize) as usize;
        self.select(index);
    }

    /// Selects the first VM, or does nothing when the list is empty.
    pub fn select_first(&mut self) {
        self.select(0);
    }

    /// Selects the last VM, or does nothing when the list is empty.
    pub fn select_last(&mut self) {
        if !self.items.is_empty() {
            self.select(self.items.len() - 1);
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = self.items.get(index).map(|vm| vm.id);
        self.last_index = index;
    }
}
