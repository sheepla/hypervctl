//! The help overlay (spec 3.3): every key of the keymap tables, in the same
//! order the status bar shows the hints. Adding a row to the table is enough
//! for it to appear here.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::keymap::{self, Context};
use crate::tui::store::Focus;

/// Draws the help over the panes, if it is open.
pub fn render(frame: &mut Frame, area: Rect, focus: Focus) {
    let mut lines = vec![Line::raw("Keys of the focused pane"), Line::raw("")];
    let pane = match focus {
        Focus::List => Context::List,
        Focus::Detail => Context::Detail,
    };
    lines.extend(hint_lines(pane));
    lines.push(Line::raw(""));
    lines.push(Line::raw("Keys of the menu"));
    lines.push(Line::raw(""));
    lines.extend(hint_lines(Context::Menu));
    lines.push(Line::raw(""));
    lines.push(Line::raw("Keys of the confirmation dialog"));
    lines.push(Line::raw(""));
    lines.extend(hint_lines(Context::Confirm));

    let height = (lines.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let width = 52u16.min(area.width.saturating_sub(2)).max(24);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().title("Help");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn hint_lines(context: Context) -> Vec<Line<'static>> {
    keymap::hints(context).into_iter().map(Line::raw).collect()
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [_, vertical, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, horizontal, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .areas(vertical);
    horizontal
}
