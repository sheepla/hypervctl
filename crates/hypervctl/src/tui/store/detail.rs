//! The state of the detail pane: which tab is up and the data of at most one
//! VM (design 5.3).

use hypervctl_core::{VmDetail, VmId};

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

    /// The index of the tab in the tab bar.
    pub const fn index(self) -> usize {
        match self {
            Self::Details => 0,
            Self::Checkpoints => 1,
        }
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
    /// selection did not change.
    pub fn follow(&mut self, selected: Option<VmId>) {
        if self.for_vm != selected {
            self.for_vm = selected;
            self.detail = None;
            self.scroll = 0;
        }
    }
}
