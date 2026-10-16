//! The layout: the VM list on top, the detail pane below it, the status bar
//! last (spec 3.2). Below the minimum size the TUI draws one notice instead of
//! a broken layout (spec 3.1). Popups are drawn over the detail pane, the help
//! over everything.

use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::features;
use crate::tui::store::{Overlay, State, UiState};

pub mod layout;
pub mod monitor;
pub mod scroll;
pub mod status;
pub mod text;
pub mod tree;

/// Draws one frame of `state` over the whole terminal.
pub fn draw(frame: &mut Frame, state: &State) {
    let area = frame.area();
    if state.ui.too_small() {
        frame.render_widget(
            Paragraph::new(format!(
                "Terminal too small (need {}x{})",
                UiState::MIN_WIDTH,
                UiState::MIN_HEIGHT
            )),
            area,
        );
        return;
    }
    // The pane heights come from the reducer's copy of the layout, so the
    // scroll arithmetic and the drawing can never disagree (design 5.11).
    let panes = state.ui.layout;
    features::vm_list::render(frame, panes.list, state);
    features::detail::render(frame, panes.detail, state);
    features::status_bar::render(frame, panes.status, state);
    match &state.overlay {
        Overlay::Menu(_) => features::menu::render_menu(frame, panes.detail, state),
        Overlay::Confirm(_) => features::menu::render_confirm(frame, panes.detail, state),
        Overlay::Help => features::help::render(frame, area, state),
        Overlay::None => {}
    }
}
