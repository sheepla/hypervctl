//! The layout: the VM list on top, the detail pane below it, the status bar
//! last (spec 3.2). Below the minimum size the TUI draws one notice instead of
//! a broken layout (spec 3.1).

use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::features;
use crate::tui::store::{State, UiState};

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
    let [list_area, detail_area, status_area] = Layout::vertical([
        Constraint::Percentage(45),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    features::vm_list::render(frame, list_area, state);
    features::detail::render(frame, detail_area, state);
    features::status_bar::render(frame, status_area, state);
}
