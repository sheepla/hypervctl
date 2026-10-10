//! The VM list pane (spec 3.2, design 5.9): the same columns as `vm list`, a
//! marker on the selected row, and the state spelled out so that nothing
//! depends on colour alone.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, List, ListItem, ListState};
use ratatui::Frame;

use hypervctl_core::{VmState, VmSummary};

use crate::cli::output::format;
use crate::cli::output::table::{truncate, width};
use crate::cli::output::views::state_label;
use crate::tui::store::State;

/// Everything right of the name column: the marker, the widest state word,
/// and the CPU, memory and uptime columns, with their gaps.
const RIGHT_OF_NAME: u16 = 2 + 11 + 2 + 5 + 2 + 8 + 2 + 8;

/// The four spinner frames (spec 3.5).
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

/// Renders the list into `area`, scrolling so that the selection stays visible.
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let name_width = usize::from(area.width.saturating_sub(RIGHT_OF_NAME + 2).max(1));
    let items: Vec<ListItem> = state
        .vm_list
        .items
        .iter()
        .map(|vm| {
            let busy = state.ops.label(vm.id).map(str::to_string);
            ListItem::new(line(vm, name_width, busy.as_deref(), state.ui.spinner))
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title("VMs"))
        .highlight_style(Style::new().reversed())
        .highlight_symbol("> ");
    let mut list_state = ListState::default().with_selected(state.vm_list.selected_index());
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// One row: name, state, CPU, memory and uptime, in that order (spec 3.2). A
/// VM with an operation in flight replaces the CPU cell with a spinner (spec 3.5).
fn line(
    vm: &VmSummary,
    name_width: usize,
    op_label: Option<&str>,
    spinner: usize,
) -> ratatui::text::Line<'static> {
    let name = truncate(&vm.name, name_width);
    let pad = " ".repeat(name_width.saturating_sub(width(&name)));
    let state = state_label(vm.state);
    let cpu = match op_label {
        Some(label) => {
            let frame = SPINNER[spinner % SPINNER.len()];
            format!("{frame} {label}")
        }
        None => match vm.cpu_usage_percent {
            Some(value) => format!("{:>5}", format::percent(value)),
            None => "    -".to_string(),
        },
    };
    let memory = match vm.memory_assigned_bytes {
        Some(value) => format!("{:>8}", format::size(value)),
        None => "       -".to_string(),
    };
    let uptime = match vm.uptime {
        Some(value) => format::uptime(value),
        None => "-".to_string(),
    };
    ratatui::text::Line::from(vec![
        ratatui::text::Span::raw(format!("{name}{pad}")),
        ratatui::text::Span::raw("  "),
        ratatui::text::Span::styled(format!("{state:<11}"), state_style(vm.state)),
        ratatui::text::Span::raw("  "),
        ratatui::text::Span::raw(cpu),
        ratatui::text::Span::raw("  "),
        ratatui::text::Span::raw(memory),
        ratatui::text::Span::raw("  "),
        ratatui::text::Span::raw(uptime),
    ])
}

/// The colour of the state word; the word itself always carries the meaning
/// (spec 3.1).
fn state_style(state: VmState) -> Style {
    match state {
        VmState::Running => Style::new().fg(Color::Green),
        VmState::Starting
        | VmState::Stopping
        | VmState::Saving
        | VmState::Pausing
        | VmState::Resuming
        | VmState::Resetting => Style::new().fg(Color::Yellow),
        VmState::Unknown(_) => Style::new().fg(Color::Red),
        _ => Style::new(),
    }
}
