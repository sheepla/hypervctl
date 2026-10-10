//! The action menu and the confirmation dialog (spec 3.4): centred popups
//! drawn over the panes. The menu greys out rows that cannot run, and the
//! dialog names the one key that confirms.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::store::menu::{CHECKPOINT_MENU, VM_MENU};
use crate::tui::store::overlay::MenuKind;
use crate::tui::store::{menu::vm_item_enabled, State};

/// The widest popup the layout is allowed to make; anything longer is still
/// readable because the text is short.
const POPUP_WIDTH: u16 = 48;
const POPUP_MAX_HEIGHT: u16 = 16;

/// Draws the open menu, if any, over the panes.
pub fn render_menu(frame: &mut Frame, area: Rect, state: &State) {
    let Some(menu) = state.overlay.menu() else {
        return;
    };
    let (title, lines) = match menu.kind {
        MenuKind::Vm => {
            let items: Vec<Line> = VM_MENU
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let enabled = vm_state_of(state, menu.vm).is_some_and(|vm_state| {
                        vm_item_enabled(*item, vm_state, state.ops.in_flight(menu.vm))
                    });
                    menu_line(item.label(), index == menu.cursor, enabled)
                })
                .collect();
            let name = state.vm_list.name_of(menu.vm).unwrap_or_default();
            (format!("Actions — {name}"), items)
        }
        MenuKind::Checkpoint => {
            let items: Vec<Line> = CHECKPOINT_MENU
                .iter()
                .enumerate()
                .map(|(index, item)| menu_line(item.label(), index == menu.cursor, true))
                .collect();
            let name = menu
                .checkpoint
                .as_ref()
                .map(|(_, name)| name.as_str())
                .unwrap_or("checkpoint");
            (format!("Checkpoint — {name}"), items)
        }
    };
    draw_popup(frame, area, &title, lines);
}

/// Draws the open confirmation dialog, if any, over the panes.
pub fn render_confirm(frame: &mut Frame, area: Rect, state: &State) {
    let Some(confirm) = state.overlay.confirm() else {
        return;
    };
    let lines = vec![
        Line::raw(confirm.question.clone()),
        Line::raw(""),
        Line::from(vec![
            Span::styled("y", Style::new().fg(Color::Green)),
            Span::raw("  run it    "),
            Span::styled("any other key", Style::new().dim()),
            Span::raw("  cancel"),
        ]),
    ];
    draw_popup(frame, area, "Confirm", lines);
}

/// One row of the menu: the label, highlighted when selected and greyed when
/// it cannot run right now.
fn menu_line(label: &str, selected: bool, enabled: bool) -> Line<'static> {
    let style = if selected {
        Style::new().reversed()
    } else if enabled {
        Style::new()
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Line::from(Span::styled(format!("{label:<20}"), style))
}

fn vm_state_of(state: &State, vm: hypervctl_core::VmId) -> Option<hypervctl_core::VmState> {
    state
        .vm_list
        .items
        .iter()
        .find(|entry| entry.id == vm)
        .map(|entry| entry.state)
}

/// A bordered box in the middle of `area`, big enough for `lines`.
fn draw_popup(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let height = (lines.len() as u16 + 2).clamp(3, POPUP_MAX_HEIGHT);
    let width = POPUP_WIDTH.min(area.width.saturating_sub(2)).max(20);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::bordered().title(title.to_string());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A rectangle of `width` by `height` centred in `area`.
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
