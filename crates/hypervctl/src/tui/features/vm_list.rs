//! The VM list pane (spec 3.2, design 5.9): the same columns as `vm list`, a
//! marker on the selected row, and the state spelled out so that nothing
//! depends on colour alone.

use ratatui::layout::{Constraint, Layout as RatLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use hypervctl_core::{VmState, VmSummary};

use crate::cli::output::format;
use crate::cli::output::table::width;
use crate::cli::output::views::state_label;
use crate::tui::store::{Focus, State};
use crate::tui::ui::layout;
use crate::tui::ui::text::take_width;

/// The four spinner frames (spec 3.5).
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

/// Renders the list into `area`, scrolling so that the selection stays
/// visible (spec 3.2).
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let columns = layout::list_columns(area.width.saturating_sub(4));
    // The rows are exactly as wide as the pane's inner width (spec 3.2),
    // unless the terminal is too narrow to keep even the floor.
    if area.width >= 62 {
        debug_assert_eq!(columns.total(), area.width.saturating_sub(4));
    }
    let block = layout::pane_block(
        list_title(state),
        top_right_label(state),
        rows_label(state),
        state.focus == Focus::List,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [header_area, body_area] =
        RatLayout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    frame.render_widget(Paragraph::new(header_line(columns)), header_area);

    // The visible window is computed by the reducer; the view only slices.
    let body_height = usize::from(body_area.height);
    let offset = state.vm_list.offset;
    let items: Vec<ListItem> = state
        .vm_list
        .items
        .iter()
        .skip(offset)
        .take(body_height)
        .map(|vm| ListItem::new(line(vm, columns, state)))
        .collect();
    let row_style = if state.focus == Focus::List {
        Style::new().reversed()
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let list = List::new(items).highlight_style(row_style);
    let selected = state
        .vm_list
        .selected_index()
        .map(|index| index.saturating_sub(offset));
    let mut list_state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, body_area, &mut list_state);
}

/// The pane title: `VMs (4)`, or with the filter once one exists (spec 3.2).
fn list_title(state: &State) -> String {
    format!("VMs ({})", state.vm_list.items.len())
}

/// `[mock]` and `stale 12s` on the top border's right end (spec 3.2).
fn top_right_label(state: &State) -> Option<String> {
    let mut parts = Vec::new();
    if state.ui.mock {
        parts.push("[mock]".to_string());
    }
    if let (Some(_), Some(since)) = (&state.vm_list.stale, state.vm_list.stale_since) {
        let age = state.ui.now.saturating_duration_since(since).as_secs();
        parts.push(format!("stale {age}s"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// ` rows 5-8 of 12 ` on the bottom border's right end, when the list is
/// scrolled (spec 3.2).
fn rows_label(state: &State) -> Option<String> {
    let total = state.vm_list.items.len();
    let body = usize::from(layout::list_body_height(state.ui.layout));
    if body == 0 || total <= body {
        return None;
    }
    let offset = state.vm_list.offset.min(total.saturating_sub(1));
    let last = (offset + body).min(total);
    Some(format!(" rows {}-{} of {total} ", offset + 1, last))
}

/// The header row: the column labels of `vm list`, NAME and STATE left, the
/// numbers right (spec 3.2).
fn header_line(columns: layout::ListColumns) -> ratatui::text::Line<'static> {
    let gap = " ".repeat(usize::from(columns.gap));
    ratatui::text::Line::from(format!(
        "{}{gap}{}{gap}{}{gap}{}{gap}{}",
        pad_right_fit("NAME", usize::from(columns.name)),
        pad_right_fit("STATE", usize::from(columns.state)),
        pad_left_fit("CPU", usize::from(columns.cpu)),
        pad_left_fit("MEMORY", usize::from(columns.memory)),
        pad_left_fit("UPTIME", usize::from(columns.uptime)),
    ))
}

/// One row: marker, name, state, CPU, memory and uptime, in that order
/// (spec 3.2). A VM with an operation in flight replaces the marker with a
/// spinner; the columns never move (anomaly 33).
fn line(
    vm: &VmSummary,
    columns: layout::ListColumns,
    state: &State,
) -> ratatui::text::Line<'static> {
    let marker = if state.ops.label(vm.id).is_some() {
        let frame = SPINNER[state.ui.spinner % SPINNER.len()];
        format!("{frame} ")
    } else if state.vm_list.selected == Some(vm.id) {
        "> ".to_string()
    } else {
        "  ".to_string()
    };
    let gap = " ".repeat(usize::from(columns.gap));
    let state_text = take_width(&state_label(vm.state), usize::from(columns.state));
    ratatui::text::Line::from(vec![
        ratatui::text::Span::raw(marker),
        ratatui::text::Span::raw(pad_right_fit(&vm.name, usize::from(columns.name))),
        ratatui::text::Span::raw(gap.clone()),
        ratatui::text::Span::styled(
            pad_right_fit(&state_text, usize::from(columns.state)),
            state_style(vm.state),
        ),
        ratatui::text::Span::raw(gap.clone()),
        ratatui::text::Span::raw(pad_left_fit(
            &match vm.cpu_usage_percent {
                Some(value) => format::percent(value),
                None => "-".to_string(),
            },
            usize::from(columns.cpu),
        )),
        ratatui::text::Span::raw(gap.clone()),
        ratatui::text::Span::raw(pad_left_fit(
            &match vm.memory_assigned_bytes {
                Some(value) => format::size(value),
                None => "-".to_string(),
            },
            usize::from(columns.memory),
        )),
        ratatui::text::Span::raw(gap),
        ratatui::text::Span::raw(pad_left_fit(
            &match vm.uptime {
                Some(value) => format::uptime(value),
                None => "-".to_string(),
            },
            usize::from(columns.uptime),
        )),
    ])
}

/// Text cut to `field` columns, padded to it (left or right, caller's
/// choice), so that every cell has exactly the width of its column.
fn pad_right_fit(text: &str, field: usize) -> String {
    let text = take_width(text, field);
    let padding = field.saturating_sub(width(&text));
    format!("{text}{}", " ".repeat(padding))
}

fn pad_left_fit(text: &str, field: usize) -> String {
    let text = take_width(text, field);
    let padding = field.saturating_sub(width(&text));
    format!("{}{text}", " ".repeat(padding))
}

/// The colour of the state word; the word itself always carries the meaning
/// (spec 3.1).
pub(crate) fn state_style(state: VmState) -> Style {
    match state {
        VmState::Running => Style::new().fg(Color::Green),
        VmState::Starting | VmState::Stopping | VmState::Busy => Style::new().fg(Color::Yellow),
        VmState::Unknown(_) => Style::new().fg(Color::Red),
        _ => Style::new(),
    }
}
