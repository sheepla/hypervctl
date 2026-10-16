//! The state of the detail pane: which tab is up and the data of at most one
//! VM (design 5.3).

use hypervctl_core::{Checkpoint, CheckpointId, VmDetail, VmId};

/// The tabs of the detail pane (spec 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Details,
    Checkpoints,
}

impl DetailTab {
    /// The tab to the right, wrapping around.
    pub const fn next(self) -> Self {
        match self {
            Self::Details => Self::Checkpoints,
            Self::Checkpoints => Self::Details,
        }
    }

    /// The tab to the left, wrapping around.
    pub const fn previous(self) -> Self {
        match self {
            Self::Details => Self::Checkpoints,
            Self::Checkpoints => Self::Details,
        }
    }
}

/// The checkpoints of one VM, with the selection kept by id the same way the
/// list keeps its own (design 5.3 and 5.6).
#[derive(Debug, Default)]
pub struct CheckpointsState {
    /// Which VM these checkpoints belong to; a result for another VM is
    /// discarded (design 5.5).
    pub for_vm: Option<VmId>,
    /// Tree order: children directly after their parent (spec 3.2).
    pub items: Vec<Checkpoint>,
    pub selected: Option<CheckpointId>,
    pub last_index: usize,
    /// A fetch is running; do not pile another one up (design 5.5).
    pub in_flight: bool,
}

impl CheckpointsState {
    /// The position of the selected checkpoint, when it is still listed.
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.items.iter().position(|entry| entry.id == id)
    }

    /// Installs a fresh list for `vm`, keeping the selection by id.
    ///
    /// Returns the name of the checkpoint that was selected and is gone, so
    /// the caller can tell the user (design 5.6).
    pub fn apply(&mut self, vm: VmId, items: Vec<Checkpoint>) -> Option<String> {
        let vanished = match self.selected {
            Some(id) if items.iter().all(|entry| entry.id != id) => self.name_of(id),
            _ => None,
        };
        self.for_vm = Some(vm);
        self.items = items;
        if self.selected.is_some() {
            if let Some(index) = self.selected_index() {
                self.last_index = index;
                return None;
            }
        }
        self.select_nearest();
        vanished
    }

    /// Moves the selection by `delta`, clamped at both ends.
    pub fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let current = self.selected_index().unwrap_or(self.last_index);
        let last = self.items.len() - 1;
        let index = (current as isize)
            .saturating_add(delta)
            .clamp(0, last as isize) as usize;
        self.selected = self.items.get(index).map(|entry| entry.id);
        self.last_index = index;
    }

    /// Jumps to the first checkpoint (`g`, spec 3.3).
    pub fn select_first(&mut self) {
        self.move_by(isize::MIN);
    }

    /// Jumps to the last checkpoint (`G`, spec 3.3).
    pub fn select_last(&mut self) {
        self.move_by(isize::MAX);
    }

    /// The selected checkpoint, if any.
    pub fn selected_entry(&self) -> Option<&Checkpoint> {
        let id = self.selected?;
        self.items.iter().find(|entry| entry.id == id)
    }

    fn select_nearest(&mut self) {
        if self.items.is_empty() {
            self.selected = None;
            self.last_index = 0;
            return;
        }
        let index = self.last_index.min(self.items.len() - 1);
        self.selected = self.items.get(index).map(|entry| entry.id);
        self.last_index = index;
    }

    fn name_of(&self, id: CheckpointId) -> Option<String> {
        self.items
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.name.clone())
    }
}

/// Everything the detail pane knows (design 5.3).
#[derive(Debug)]
pub struct DetailState {
    pub tab: DetailTab,
    /// Which VM the loaded detail belongs to. A result for any other VM is
    /// discarded (design 5.5, acceptance 10).
    pub for_vm: Option<VmId>,
    pub detail: Option<VmDetail>,
    pub checkpoints: CheckpointsState,
    /// How far the content is scrolled down (spec 3.3).
    pub scroll: u16,
}

impl DetailState {
    /// Shows the detail of `id`, dropping whatever was loaded for another VM.
    pub fn show(&mut self, id: VmId, detail: VmDetail) {
        self.for_vm = Some(id);
        self.detail = Some(detail);
        self.scroll = 0;
    }

    /// Follows the selection to a different VM, or does nothing when the
    /// selection did not change. The checkpoints follow: they belong to the
    /// selected VM (spec 3.7).
    pub fn follow(&mut self, selected: Option<VmId>) {
        if self.for_vm != selected {
            self.for_vm = selected;
            self.detail = None;
            self.checkpoints = CheckpointsState::default();
            self.scroll = 0;
        }
    }
}
