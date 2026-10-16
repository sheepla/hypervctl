//! The help overlay (spec 3.3): every key of the keymap tables, in the same
//! order the status bar shows the hints. Adding a row to the table is enough
//! for it to appear here.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::keymap::{self, Context};
use crate::tui::store::{Focus, State};
use crate::tui::ui::layout;

/// Draws the help over the whole terminal, if it is open. The scroll offset
/// comes from the reducer (design 5.11).
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let popup = layout::help_rect(area);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().title("Help");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let lines = content_lines(state.focus);
    let height = usize::from(inner.height);
    let scroll = usize::from(state.help_scroll);
    let visible: Vec<Line> = lines.iter().skip(scroll).take(height).cloned().collect();
    frame.render_widget(Paragraph::new(visible), inner);
}

/// Every line the overlay holds, so the view and the scroll clamp see the
/// same content (design 5.11).
pub fn content_lines(focus: Focus) -> Vec<Line<'static>> {
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
    lines.push(Line::raw(""));
    lines.push(Line::raw("Keys of the help"));
    lines.push(Line::raw(""));
    lines.extend(hint_lines(Context::Help));
    lines
}

fn hint_lines(context: Context) -> Vec<Line<'static>> {
    keymap::hints(context).into_iter().map(Line::raw).collect()
}
