//! The status bar (spec 3.2): the key hints of the focused pane, and the
//! message that something just happened.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::keymap;
use crate::tui::store::{status::StatusKind, State};

/// Renders the bar into `area`, which is one line high.
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let hints = keymap::hints(state.keymap_context()).join("  ");
    let mut spans = vec![Span::styled(hints, Style::new().dim())];
    // A stale list outranks the current message: it says the screen lies
    // (design 5.5).
    if let Some(stale) = &state.vm_list.stale {
        spans.push(Span::raw("  │  "));
        spans.push(Span::styled(
            format!("warning: {stale}"),
            Style::new().fg(Color::Yellow),
        ));
    } else if let Some(message) = &state.status.current {
        let style = match message.kind {
            StatusKind::Info => Style::new(),
            StatusKind::Error => Style::new().fg(Color::Red),
        };
        spans.push(Span::raw("  │  "));
        spans.push(Span::styled(message.text.clone(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
