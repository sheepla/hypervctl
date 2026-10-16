//! The detail pane (spec 3.2): the tab bar and the contents of the selected
//! VM, or a placeholder while there is nothing to show.

use ratatui::layout::{Constraint, Layout as RatLayout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::cli::output::format;
use crate::cli::output::table::width;
use crate::cli::output::views::{state_label, VmDetailView};
use crate::tui::store::{DetailTab, Focus, State};
use crate::tui::ui::layout;
use crate::tui::ui::text::{middle_ellipsis, pad_right, take_width};

use super::vm_list::state_style;

/// The label column of the body (spec 3.2: 14, the value right of it).
const LABEL_WIDTH: usize = 14;

/// Renders the pane into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let block = layout::pane_block(
        detail_title(state),
        None,
        lines_label(state),
        state.focus == Focus::Detail,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [tabs_area, body_area] =
        RatLayout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    frame.render_widget(tab_line(state), tabs_area);
    match state.detail.tab {
        DetailTab::Checkpoints => render_checkpoints(frame, body_area, state),
        DetailTab::Details => {
            // The scroll offset comes from the reducer; the view only slices.
            let lines = body_lines(state);
            let height = usize::from(body_area.height);
            let scroll = usize::from(state.detail.scroll);
            let visible: Vec<Line> = lines.iter().skip(scroll).take(height).cloned().collect();
            frame.render_widget(Paragraph::new(visible), body_area);
        }
    }
}

/// `Details   Checkpoints`, the selected tab reversed (spec 3.2).
fn tab_line(state: &State) -> Line<'static> {
    let selected = Style::new().reversed();
    let plain = Style::new();
    Line::from(vec![
        Span::styled(
            "Details",
            if state.detail.tab == DetailTab::Details {
                selected
            } else {
                plain
            },
        ),
        Span::raw("   "),
        Span::styled(
            "Checkpoints",
            if state.detail.tab == DetailTab::Checkpoints {
                selected
            } else {
                plain
            },
        ),
    ])
}

/// The pane title: `<VM name> · <State>`, or empty when the list is (spec
/// 3.2). The summary on screen is enough; the detail need not be loaded.
fn detail_title(state: &State) -> String {
    let Some(vm) = state
        .vm_list
        .selected
        .and_then(|id| state.vm_list.items.iter().find(|vm| vm.id == id))
    else {
        return String::new();
    };
    format!("{} · {}", vm.name, state_label(vm.state))
}

/// ` lines 1-12 of 17 ` on the bottom border's right end, when the body is
/// scrolled (spec 3.2).
fn lines_label(state: &State) -> Option<String> {
    if state.detail.tab != DetailTab::Details {
        return None;
    }
    let total = body_lines(state).len();
    let body = usize::from(layout::detail_body_height(state.ui.layout));
    if body == 0 || total <= body {
        return None;
    }
    let scroll = usize::from(state.detail.scroll).min(total.saturating_sub(1));
    let last = (scroll + body).min(total);
    Some(format!(" lines {}-{} of {total} ", scroll + 1, last))
}

/// The lines under the tab bar on the Details tab, in the groups the
/// specification lists (spec 3.2).
pub fn body_lines(state: &State) -> Vec<Line<'static>> {
    if state.vm_list.items.is_empty() {
        return vec![Line::raw("No VMs")];
    }
    match &state.detail.detail {
        Some(detail) => detail_lines(VmDetailView::from_detail(detail.clone()), state),
        None => vec![Line::raw("Loading...")],
    }
}

/// One group of body rows, or a blank line when there is nothing to show.
type Row = (String, String, Option<Style>, bool);

fn detail_lines(view: VmDetailView, state: &State) -> Vec<Line<'static>> {
    let label_field = u16::try_from(LABEL_WIDTH).unwrap_or(u16::MAX) + 1;
    let value_width = usize::from(
        state
            .ui
            .layout
            .detail
            .width
            .saturating_sub(4)
            .saturating_sub(label_field),
    );
    let state_style = state
        .vm_list
        .selected
        .and_then(|id| state.vm_list.items.iter().find(|vm| vm.id == id))
        .map(|vm| state_style(vm.state))
        .unwrap_or_default();
    let group1: Vec<Row> = vec![
        (
            "State".to_string(),
            view.state_label.clone(),
            Some(state_style),
            false,
        ),
        (
            "Generation".to_string(),
            view.generation
                .map(|g| g.to_string())
                .unwrap_or_else(|| "-".to_string()),
            None,
            false,
        ),
        (
            "Uptime".to_string(),
            view.uptime_seconds
                .map(|seconds| format::uptime(std::time::Duration::from_secs(seconds)))
                .unwrap_or_else(|| "-".to_string()),
            None,
            false,
        ),
        (
            "CPU".to_string(),
            view.cpu_percent
                .map(format::percent)
                .unwrap_or_else(|| "-".to_string()),
            None,
            false,
        ),
    ];
    let dynamic = if view.memory.dynamic_enabled {
        match (view.memory.min_bytes, view.memory.max_bytes) {
            (Some(min), Some(max)) => {
                format!(" (dynamic {}-{})", format::size(min), format::size(max))
            }
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let group2: Vec<Row> = vec![
        (
            "Processors".to_string(),
            view.processor_count.to_string(),
            None,
            false,
        ),
        (
            "Memory".to_string(),
            format!("{}{dynamic}", format::size(view.memory.startup_bytes)),
            None,
            false,
        ),
    ];
    let mut group3: Vec<Row> = Vec::new();
    for (index, disk) in view.disks.iter().enumerate() {
        let controller = format!(" ({})", disk.controller_label);
        let path_budget = value_width.saturating_sub(width(&controller));
        let path = if width(&disk.path) > path_budget {
            middle_ellipsis(&disk.path, path_budget)
        } else {
            disk.path.clone()
        };
        group3.push((
            format!("Disk {index}"),
            format!("{path}{controller}"),
            None,
            true,
        ));
    }
    for (index, nic) in view.nics.iter().enumerate() {
        let mut value = nic
            .switch_name
            .clone()
            .unwrap_or_else(|| "not connected".to_string());
        if let Some(mac) = &nic.mac_address {
            value.push_str(", ");
            value.push_str(mac);
        }
        group3.push((format!("Nic {index}"), value, None, false));
    }
    match &view.integration_services {
        None => group3.push(("Services".to_string(), "-".to_string(), None, false)),
        Some(services) => {
            for service in services {
                group3.push((
                    format!("Service: {}", service.name),
                    format!(
                        "{}, {}",
                        if service.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        service.status_label
                    ),
                    None,
                    false,
                ));
            }
        }
    }
    let group4: Vec<Row> = vec![
        (
            "Checkpoints".to_string(),
            view.checkpoint_count.to_string(),
            None,
            false,
        ),
        ("Config".to_string(), view.config_path.clone(), None, true),
        (
            "Notes".to_string(),
            view.notes.clone().unwrap_or_else(|| "-".to_string()),
            None,
            false,
        ),
    ];

    let mut lines = Vec::new();
    for group in [group1, group2, group3, group4] {
        if group.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        for (label, value, style, is_path) in group {
            let value = if is_path {
                middle_ellipsis(&value, value_width)
            } else {
                take_width(&value, value_width)
            };
            lines.push(Line::from(vec![
                Span::raw(format!("{} ", pad_right(&label, LABEL_WIDTH))),
                Span::styled(value, style.unwrap_or_default()),
            ]));
        }
    }
    lines
}

/// The checkpoints of the selected VM as a list (spec 3.2).
fn render_checkpoints(frame: &mut Frame, area: Rect, state: &State) {
    let checkpoints = &state.detail.checkpoints;
    let items: Vec<ListItem> = if checkpoints.in_flight && checkpoints.items.is_empty() {
        vec![ListItem::new(Line::raw("Loading..."))]
    } else if checkpoints.items.is_empty() {
        vec![ListItem::new(Line::raw("No checkpoints"))]
    } else {
        checkpoints
            .items
            .iter()
            .map(|entry| ListItem::new(Line::raw(entry.name.clone())))
            .collect()
    };
    let row_style = if state.focus == Focus::Detail {
        Style::new().reversed()
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let list = List::new(items).highlight_style(row_style);
    let mut list_state = ListState::default().with_selected(checkpoints.selected_index());
    frame.render_stateful_widget(list, area, &mut list_state);
}
