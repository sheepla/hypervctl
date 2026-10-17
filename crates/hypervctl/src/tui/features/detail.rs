//! The detail pane (spec 3.2): the tab bar and the contents of the selected
//! VM, or a placeholder while there is nothing to show.

use ratatui::layout::{Constraint, Layout as RatLayout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Chart, Dataset, GraphType, List, ListItem, ListState, Paragraph,
};
use ratatui::Frame;

use hypervctl_core::{Checkpoint, CheckpointKind, VmState};
use ratatui_image::Image;

use crate::cli::output::format;
use crate::cli::output::table::width;
use crate::cli::output::views::{state_label, VmDetailView};
use crate::tui::store::{DetailTab, Focus, State};
use crate::tui::ui::text::{middle_ellipsis, pad_fit, pad_right, take_width};
use crate::tui::ui::tree::{tree_rows, TreeRow};
use crate::tui::ui::{layout, monitor};

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
        DetailTab::Checkpoints => {
            // The blank row under the tab bar, as the frame of spec 3.8 shows.
            let [_blank, list_area] =
                RatLayout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body_area);
            render_checkpoints(frame, list_area, state);
        }
        DetailTab::Monitor => render_monitor(frame, body_area, state),
        DetailTab::Thumbnail => render_thumbnail(frame, body_area, state),
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

/// ` [1] Details `, ` [2] Checkpoints `, ` [3] Monitor `, ` [4] Thumbnail `,
/// the selected tab reversed (spec 3.2; `Monitor` since M12). The number is the
/// key that selects the tab, padded on both sides.
fn tab_line(state: &State) -> Line<'static> {
    let selected = Style::new().reversed();
    let plain = Style::new();
    let tabs = [
        (1, "Details", DetailTab::Details),
        (2, "Checkpoints", DetailTab::Checkpoints),
        (3, "Monitor", DetailTab::Monitor),
        (4, "Thumbnail", DetailTab::Thumbnail),
    ];
    let spans: Vec<Span<'static>> = tabs
        .into_iter()
        .map(|(key, label, tab)| {
            let style = if state.detail.tab == tab {
                selected
            } else {
                plain
            };
            Span::styled(format!(" [{key}] {label} "), style)
        })
        .collect();
    Line::from(spans)
}

/// The pane title: `<VM name> · <State>`, or empty when the selection is
/// filtered away (spec 3.2). The summary on screen is enough; the detail need
/// not be loaded.
fn detail_title(state: &State) -> String {
    let Some(vm) = state.vm_list.selected_summary() else {
        return String::new();
    };
    format!("{} · {}", vm.name, state_label(vm.state))
}

/// ` lines 1-12 of 17 ` on the bottom border's right end while the Details
/// body is scrolled, ` rows 1-9 of 14 ` while the checkpoints tree is (spec
/// 3.2 and 3.8).
fn lines_label(state: &State) -> Option<String> {
    let body = usize::from(layout::detail_body_height(state.ui.layout));
    if body == 0 {
        return None;
    }
    match state.detail.tab {
        DetailTab::Details => {
            let total = body_lines(state).len();
            if total <= body {
                return None;
            }
            let scroll = usize::from(state.detail.scroll).min(total.saturating_sub(1));
            let last = (scroll + body).min(total);
            Some(format!(" lines {}-{} of {total} ", scroll + 1, last))
        }
        DetailTab::Checkpoints => {
            let total = state.detail.checkpoints.items.len();
            if total <= body {
                return None;
            }
            let offset = state.detail.checkpoints.offset.min(total.saturating_sub(1));
            let last = (offset + body).min(total);
            Some(format!(" rows {}-{} of {total} ", offset + 1, last))
        }
        // The graphs never scroll, so there is no range to report (design
        // 5.12). Nor does the thumbnail (design 5.13).
        DetailTab::Monitor | DetailTab::Thumbnail => None,
    }
}

/// The lines under the tab bar on the Details tab, in the groups the
/// specification lists (spec 3.2). Nothing selected under a filter that
/// matched nothing says so (spec 3.3).
pub fn body_lines(state: &State) -> Vec<Line<'static>> {
    if state.vm_list.items.is_empty() {
        return vec![Line::raw("No VMs")];
    }
    if state.vm_list.selected.is_none() {
        return vec![Line::raw("No matching VMs")];
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
        .selected_summary()
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

/// The checkpoints of the selected VM as a tree with a column header (spec
/// 3.8): the branch lines come from the pure rows of [`tree_rows`], the
/// reducer's offset scrolls the window.
fn render_checkpoints(frame: &mut Frame, area: Rect, state: &State) {
    let checkpoints = &state.detail.checkpoints;
    if checkpoints.in_flight && checkpoints.items.is_empty() {
        frame.render_widget(Paragraph::new(Line::raw("Loading...")), area);
        return;
    }
    if checkpoints.items.is_empty() {
        frame.render_widget(Paragraph::new(Line::raw("No checkpoints")), area);
        return;
    }
    let columns = layout::checkpoint_columns(area.width.saturating_sub(4));
    // The rows are exactly as wide as the pane's inner width (spec 3.8),
    // unless the terminal is too narrow to keep even the floor.
    if area.width >= 49 {
        debug_assert_eq!(columns.total(), area.width.saturating_sub(4));
    }
    let [header_area, body_area] =
        RatLayout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(Paragraph::new(checkpoint_header(columns)), header_area);

    // The scroll window comes from the reducer; the view only slices (spec 3.8).
    let rows = tree_rows(&checkpoints.items);
    let body_height = usize::from(body_area.height);
    let offset = checkpoints.offset;
    let items: Vec<ListItem> = checkpoints
        .items
        .iter()
        .zip(rows)
        .skip(offset)
        .take(body_height)
        .map(|(entry, row)| ListItem::new(checkpoint_line(entry, &row, columns, state)))
        .collect();
    let row_style = if state.focus == Focus::Detail {
        Style::new().reversed()
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let list = List::new(items).highlight_style(row_style);
    let selected = checkpoints
        .selected_index()
        .map(|index| index.saturating_sub(offset));
    let mut list_state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, body_area, &mut list_state);
}

/// `NAME   TYPE   CREATED`, the column labels of spec 3.8.
fn checkpoint_header(columns: layout::CheckpointColumns) -> Line<'static> {
    let gap = " ".repeat(usize::from(columns.gap));
    Line::from(format!(
        "{}{gap}{}{gap}{}",
        pad_fit("NAME", usize::from(columns.name)),
        pad_fit("TYPE", usize::from(columns.kind)),
        pad_fit("CREATED", usize::from(columns.created)),
    ))
}

/// One tree row: the marker column, the branch and name, the kind and the
/// local creation time (spec 3.8). A row never shifts its columns.
fn checkpoint_line(
    entry: &Checkpoint,
    row: &TreeRow,
    columns: layout::CheckpointColumns,
    state: &State,
) -> Line<'static> {
    let marker = if state.detail.checkpoints.selected == Some(entry.id) {
        "> "
    } else {
        "  "
    };
    let gap = " ".repeat(usize::from(columns.gap));
    let name = format!("{}{}", row.prefix(), entry.name);
    Line::from(vec![
        Span::raw(marker),
        Span::raw(pad_fit(&name, usize::from(columns.name))),
        Span::raw(gap.clone()),
        Span::raw(pad_fit(
            &checkpoint_kind_text(entry.kind),
            usize::from(columns.kind),
        )),
        Span::raw(gap),
        Span::raw(pad_fit(
            &format::local_minutes(entry.created),
            usize::from(columns.created),
        )),
    ])
}

/// The TYPE column: lowercase, as spec 3.8 writes it; a value the backend did
/// not map keeps its number.
fn checkpoint_kind_text(kind: CheckpointKind) -> String {
    match kind {
        CheckpointKind::Standard => "standard".to_string(),
        CheckpointKind::Production => "production".to_string(),
        CheckpointKind::Unknown(raw) => format!("unknown({raw})"),
    }
}

/// The two graphs of the Monitor tab (spec 3.9): CPU on top, the assigned
/// memory below. Without samples of its own the pane says so instead of
/// drawing an empty frame (design 5.12).
fn render_monitor(frame: &mut Frame, area: Rect, state: &State) {
    let Some(vm) = state.vm_list.selected else {
        frame.render_widget(Paragraph::new(Line::raw("No VM selected")), area);
        return;
    };
    let Some(ring) = state.history.ring(vm).filter(|ring| !ring.is_empty()) else {
        frame.render_widget(Paragraph::new(Line::raw("No samples yet")), area);
        return;
    };
    let window = monitor::window(state.ui.refresh);
    // The right edge is the newest sample, so a drawn frame never depends on
    // the wall clock (design 5.12).
    let now = ring
        .newest()
        .unwrap_or_else(|| unreachable!("the ring was just checked non-empty"))
        .at;
    let [cpu_area, memory_area] =
        RatLayout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    render_chart(
        frame,
        cpu_area,
        "CPU %",
        &monitor::series(ring, now, window, monitor::cpu),
        ChartAxis {
            y_max: 100.0,
            y_labels: ["0".to_string(), "50".to_string(), "100".to_string()],
            // The bottom graph carries the time axis alone (design 5.12).
            x_labels: None,
            window,
        },
    );
    let memory_max = monitor::memory_axis_max(ring, now, window);
    render_chart(
        frame,
        memory_area,
        "Memory assigned",
        &monitor::series(ring, now, window, monitor::memory),
        ChartAxis {
            y_max: memory_max,
            y_labels: [
                format::size(0),
                format::size((memory_max / 2.0) as u64),
                format::size(memory_max as u64),
            ],
            x_labels: Some(monitor::x_labels(window)),
            window,
        },
    );
}

/// The Thumbnail tab (spec 3.10): the console image of the selected VM, or the
/// reason there is none. The effect already decoded and sized the image; the
/// view only draws it (design 5.13).
fn render_thumbnail(frame: &mut Frame, area: Rect, state: &State) {
    if !state.ui.image_supported {
        frame.render_widget(
            Paragraph::new(Line::raw("Thumbnails are not supported by this terminal")),
            area,
        );
        return;
    }
    let Some(summary) = state.vm_list.selected_summary() else {
        let message = if state.vm_list.items.is_empty() {
            "No VMs"
        } else {
            "No matching VMs"
        };
        frame.render_widget(Paragraph::new(Line::raw(message)), area);
        return;
    };
    if summary.state != VmState::Running {
        frame.render_widget(
            Paragraph::new(Line::raw(format!(
                "No preview (VM is {})",
                state_label(summary.state)
            ))),
            area,
        );
        return;
    }
    // The image belongs to the VM on screen and to this pane size; anything
    // else is stale and waits for its replacement (spec 3.10).
    let current = state.thumbnail.for_vm == state.vm_list.selected
        && state.thumbnail.cells == Some(layout::thumbnail_cells(state.ui.layout));
    match state.thumbnail.protocol.as_ref().filter(|_| current) {
        Some(image) => {
            let widget = Image::new(image.protocol()).allow_clipping(true);
            frame.render_widget(widget, area);
        }
        // Nothing yet, or a failed fetch: the pane says so until the next try
        // (spec 3.10).
        None => frame.render_widget(Paragraph::new(Line::raw("Loading...")), area),
    }
}

/// The axes one graph is drawn with (design 5.12).
struct ChartAxis {
    y_max: f64,
    y_labels: [String; 3],
    x_labels: Option<[String; 2]>,
    window: std::time::Duration,
}

/// One graph: the title row, the y labels down the left, the optional x
/// labels along the bottom, and one [`Dataset`] per unbroken run of samples,
/// so a gap in the values stays a gap in the line (design 5.12).
fn render_chart(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    segments: &[monitor::Segment],
    axis: ChartAxis,
) {
    if area.height < 3 || area.width < 4 {
        return;
    }
    // Values outside their axis are clamped instead of drawn off the chart.
    let points: Vec<Vec<(f64, f64)>> = segments
        .iter()
        .map(|segment| {
            segment
                .points
                .iter()
                .map(|&(x, y)| (x, y.clamp(0.0, axis.y_max)))
                .collect()
        })
        .collect();
    let datasets: Vec<Dataset> = points
        .iter()
        .map(|data| {
            Dataset::default()
                .marker(Marker::Dot)
                .graph_type(GraphType::Line)
                .data(data)
        })
        .collect();
    // The x bounds always run the window; only the top chart leaves off the
    // labels (design 5.12). Without the bounds the default `[0.0, 0.0]`
    // would squash every point into nothing.
    let mut x_axis = Axis::default().bounds([0.0, axis.window.as_secs_f64()]);
    if let Some(labels) = axis.x_labels {
        x_axis = x_axis.labels(labels);
    }
    let chart = Chart::new(datasets)
        .block(Block::default().title(title.to_string()))
        .x_axis(x_axis)
        .y_axis(
            Axis::default()
                .bounds([0.0, axis.y_max])
                .labels(axis.y_labels),
        );
    frame.render_widget(chart, area);
}
