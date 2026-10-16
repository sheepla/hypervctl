//! The state of the VM list pane: the sorted items, the selection kept by id
//! (design 5.3 and 5.6), the filter and the visible rows it leaves (design
//! 5.11, spec 3.3), and the flags that keep polling from piling up (design
//! 5.5).

use std::time::Instant;

use hypervctl_core::{VmId, VmSummary};

/// The list filter (design 5.3, spec 3.3).
#[derive(Debug, Default)]
pub struct FilterState {
    /// What the VM names are matched against, case-insensitively.
    pub text: String,
    /// Whether the input line is up: keys type characters instead of driving
    /// the panes until `Enter` or `Esc` closes it (spec 3.3).
    pub editing: bool,
}

/// Everything the list pane knows (design 5.3).
#[derive(Debug, Default)]
pub struct VmListState {
    /// Sorted by name, then id (spec 3.2).
    pub items: Vec<VmSummary>,
    /// The active filter (design 5.3).
    pub filter: FilterState,
    /// Indices into `items` of the rows the filter leaves on screen; the
    /// selection is always one of these, or none (spec 3.3). The fetching is
    /// never filtered: only the display is (spec 3.3).
    pub visible: Vec<usize>,
    /// The selected VM, by id so a refresh never moves the selection to
    /// another VM (spec 3.6).
    pub selected: Option<VmId>,
    /// Where the selection was among the visible rows, so that a vanished VM
    /// falls back to the nearest row (design 5.6).
    pub last_index: usize,
    /// A fetch is running; a tick must not start a second one (design 5.5).
    pub refresh_in_flight: bool,
    /// Something changed while a fetch was running: fetch once more after it.
    pub refresh_again: bool,
    /// The last refresh failed; the list on screen is out of date (design 5.5).
    pub stale: Option<String>,
    /// When the last refresh failed, so the title can show `stale 12s`
    /// (spec 3.2). Cleared with `stale`.
    pub stale_since: Option<Instant>,
    /// The first visible row on screen (spec 3.2). Kept by the reducer with the
    /// same scroll arithmetic the view uses.
    pub offset: usize,
}

impl VmListState {
    /// The position of the selected VM among the visible rows, when it is
    /// still shown.
    pub fn visible_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.visible
            .iter()
            .position(|&index| self.items.get(index).is_some_and(|vm| vm.id == id))
    }

    /// The selected VM's summary, when it is still in the list.
    pub fn selected_summary(&self) -> Option<&VmSummary> {
        let id = self.selected?;
        self.items.iter().find(|vm| vm.id == id)
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
        self.stale_since = None;
        self.recompute_visible();
        if self.visible_index().is_some() {
            self.last_index = self.visible_index().unwrap_or(0);
            return vanished;
        }
        if self.visible.is_empty() {
            self.selected = None;
            self.last_index = 0;
            return vanished;
        }
        // A vanished selection falls back to the nearest visible row (spec
        // 3.6); the filter only ever hides rows of a list that is installed.
        let index = self.last_index.min(self.visible.len() - 1);
        self.selected = self.visible.get(index).map(|&index| self.items[index].id);
        self.last_index = index;
        vanished
    }

    /// Recomputes the visible rows from the current filter text (spec 3.3):
    /// a case-insensitive substring match on the VM name.
    pub fn recompute_visible(&mut self) {
        let needle = self.filter.text.to_lowercase();
        self.visible = if needle.is_empty() {
            (0..self.items.len()).collect()
        } else {
            self.items
                .iter()
                .enumerate()
                .filter(|(_, vm)| vm.name.to_lowercase().contains(&needle))
                .map(|(index, _)| index)
                .collect()
        };
    }

    /// Moves the selection to the first visible row when the filter just hid
    /// it, or does nothing (spec 3.3).
    pub fn select_first_visible(&mut self) {
        if self.visible_index().is_some() {
            return;
        }
        self.selected = self.visible.first().map(|&index| self.items[index].id);
        self.last_index = 0;
    }

    /// The name of `id`, when it is still in the list.
    pub fn name_of(&self, id: VmId) -> Option<String> {
        self.items
            .iter()
            .find(|vm| vm.id == id)
            .map(|vm| vm.name.clone())
    }

    /// Moves the selection by `delta` among the visible rows, clamped at both
    /// ends (spec 3.3).
    pub fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.visible_index().unwrap_or(self.last_index);
        let last = self.visible.len() - 1;
        let target = (current as isize + delta).clamp(0, last as isize) as usize;
        self.select_visible(target);
    }

    /// Selects the first visible VM, or does nothing when none is visible.
    pub fn select_first(&mut self) {
        self.select_visible(0);
    }

    /// Selects the last visible VM, or does nothing when none is visible.
    pub fn select_last(&mut self) {
        if !self.visible.is_empty() {
            self.select_visible(self.visible.len() - 1);
        }
    }

    fn select_visible(&mut self, index: usize) {
        self.selected = self.visible.get(index).map(|&index| self.items[index].id);
        self.last_index = index;
    }
}
