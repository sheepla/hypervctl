//! The action menu and the confirmation dialog (spec 3.4): centred popups
//! drawn over the detail pane. The menu greys out rows that cannot run, and
//! the dialog names the one key that confirms.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};
use ratatui::Frame;

use hypervctl_core::{VmId, VmState};

use crate::cli::output::table::width;
use crate::tui::store::menu::{
    checkpoint_item_enabled, enabled_states_text, vm_item_enabled, CHECKPOINT_MENU, VM_MENU,
};
use crate::tui::store::overlay::MenuKind;
use crate::tui::store::State;
use crate::tui::ui::layout;

/// The marker column of a menu row (spec 3.4).
const MARKER: usize = 2;

/// Draws the open menu, if any, over the detail pane (anomaly 28).
pub fn render_menu(frame: &mut Frame, detail: Rect, state: &State) {
    let Some(menu) = state.overlay.menu() else {
        return;
    };
    let vm_state = vm_state_of(state, menu.vm);
    let in_flight = state.ops.in_flight(menu.vm);
    let (title, busy_line, lines, label_field, state_field) = match menu.kind {
        MenuKind::Vm => {
            let name = state.vm_list.name_of(menu.vm).unwrap_or_default();
            let rows: Vec<(String, bool, Option<String>)> = VM_MENU
                .iter()
                .map(|item| {
                    let enabled = vm_state
                        .is_some_and(|vm_state| vm_item_enabled(*item, vm_state, in_flight));
                    // A grey row says where it would run: `Resume Paused`
                    // (spec 3.4, from the very table the CLI uses).
                    let state_text = if enabled {
                        None
                    } else {
                        Some(enabled_states_text(item.op_kind()))
                    };
                    (item.label().to_string(), enabled, state_text)
                })
                .collect();
            let state_field = rows
                .iter()
                .filter_map(|(_, _, text)| text.as_ref())
                .map(|text| width(text))
                .max()
                .unwrap_or(0);
            let fields = label_field(VM_MENU.iter().map(|item| item.label()));
            (
                format!("Actions · {name}"),
                busy_line(vm_state, in_flight),
                rows,
                fields,
                state_field,
            )
        }
        MenuKind::Checkpoint => {
            let name = menu
                .checkpoint
                .as_ref()
                .map(|(_, name)| name.as_str())
                .unwrap_or("checkpoint");
            let rows: Vec<(String, bool, Option<String>)> = CHECKPOINT_MENU
                .iter()
                .map(|item| {
                    let enabled = checkpoint_item_enabled(
                        *item,
                        vm_state,
                        menu.checkpoint.is_some(),
                        in_flight,
                    );
                    let state_text = if enabled {
                        None
                    } else {
                        Some(enabled_states_text(item.op_kind()))
                    };
                    (item.label().to_string(), enabled, state_text)
                })
                .collect();
            let state_field = rows
                .iter()
                .filter_map(|(_, _, text)| text.as_ref())
                .map(|text| width(text))
                .max()
                .unwrap_or(0);
            let fields = label_field(CHECKPOINT_MENU.iter().map(|item| item.label()));
            (
                format!("Checkpoint · {name}"),
                String::new(),
                rows,
                fields,
                state_field,
            )
        }
    };

    let content_width = MARKER + label_field + state_field.max(1);
    let mut lines_out: Vec<Line> = Vec::new();
    if !busy_line.is_empty() {
        lines_out.push(Line::styled(busy_line, Style::new().fg(Color::Yellow)));
    }
    for (index, (label, enabled, state_text)) in lines.iter().enumerate() {
        lines_out.push(menu_line(
            label,
            index == menu.cursor,
            *enabled,
            state_text.as_deref(),
            label_field,
            state_field,
        ));
    }
    let content_height = lines_out.len();
    let popup = layout::popup_rect(
        detail,
        content_width.min(u16::MAX as usize) as u16,
        content_height.min(u16::MAX as usize) as u16,
    );
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(title);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines_out), inner);
}

/// Draws the open confirmation dialog, if any, over the detail pane.
pub fn render_confirm(frame: &mut Frame, detail: Rect, state: &State) {
    let Some(confirm) = state.overlay.confirm() else {
        return;
    };
    let mut lines = vec![Line::raw(confirm.question.clone())];
    if let Some(note) = &confirm.note {
        lines.push(Line::raw(note.clone()));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("y", Style::new().fg(Color::Green)),
        Span::raw(" run    "),
        Span::styled("any other key", Style::new().dim()),
        Span::raw(" cancel"),
    ]));
    let widest = lines
        .iter()
        .map(|line| width(&line.to_string()))
        .max()
        .unwrap_or(0);
    let popup = layout::popup_rect(
        detail,
        widest.min(u16::MAX as usize) as u16,
        lines.len() as u16,
    );
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title("Confirm");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row of the menu: the `> ` marker, the label, and the states in which
/// the row would run, right-aligned and greyed when it cannot run now.
fn menu_line(
    label: &str,
    selected: bool,
    enabled: bool,
    state_text: Option<&str>,
    label_field: usize,
    state_field: usize,
) -> Line<'static> {
    let marker = if selected { "> " } else { "  " };
    let mut text = format!("{marker}{label}");
    let used = width(&text);
    let total = MARKER + label_field + state_field.max(1);
    let padding = total
        .saturating_sub(used)
        .saturating_sub(width(state_text.unwrap_or("")));
    text.push_str(&" ".repeat(padding));
    if let Some(state_text) = state_text {
        text.push_str(state_text);
    }
    let style = if selected {
        Style::new().reversed()
    } else if enabled {
        Style::new()
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Line::from(Span::styled(text, style))
}

/// The widest label, plus one column of air (spec 3.4's `Create checkpoint`).
fn label_field(labels: impl Iterator<Item = &'static str>) -> usize {
    labels.map(width).max().unwrap_or(0) + 1
}

/// The line under the title when nothing can run: the VM is mid-transition or
/// an operation is already under way (spec 3.4).
fn busy_line(vm_state: Option<VmState>, in_flight: bool) -> String {
    match vm_state {
        Some(VmState::Unknown(_)) => "VM state is unknown".to_string(),
        Some(VmState::Starting | VmState::Stopping | VmState::Busy) => "VM is busy".to_string(),
        _ if in_flight => "VM is busy".to_string(),
        _ => String::new(),
    }
}

fn vm_state_of(state: &State, vm: VmId) -> Option<VmState> {
    state
        .vm_list
        .items
        .iter()
        .find(|entry| entry.id == vm)
        .map(|entry| entry.state)
}
