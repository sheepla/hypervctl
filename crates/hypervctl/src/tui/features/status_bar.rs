//! The status bar (spec 3.2): the key hints of the focused pane, and the
//! message that something just happened.

use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::keymap;
use crate::tui::store::status::StatusKind;
use crate::tui::store::State;
use crate::tui::ui::status::{compose, compose_filter, Message};

/// Renders the bar into `area`, which is one line high. While the filter
/// input is up, its prompt replaces the key hints (spec 3.3).
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let message = current_message(state);
    let line = if state.vm_list.filter.editing {
        compose_filter(area.width, &state.vm_list.filter.text, message.as_ref())
    } else {
        let hints = keymap::hints(state.keymap_context());
        compose(area.width, &hints, message.as_ref())
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// The one message the bar shows, by priority (spec 3.5): a stale list
/// outranks everything, then errors and notifications, then the first
/// operation still running.
fn current_message(state: &State) -> Option<Message> {
    // A stale list is always shown, even over a newer error: it says the
    // whole screen is behind (design 5.5).
    if let Some(stale) = &state.vm_list.stale {
        return Some(Message::Stale(stale.clone()));
    }
    if let Some(current) = &state.status.current {
        return Some(match current.kind {
            StatusKind::Error => Message::Error(current.text.clone()),
            StatusKind::Info => Message::Info(current.text.clone()),
        });
    }
    // No message yet: the oldest thing still running, in list order so the
    // choice is stable between frames (spec 3.5).
    state.vm_list.items.iter().find_map(|vm| {
        state
            .ops
            .label(vm.id)
            .map(|label| Message::Info(format!("{}: {label}...", vm.name)))
    })
}
