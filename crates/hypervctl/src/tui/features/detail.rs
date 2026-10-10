//! The detail pane (spec 3.2): the tab bar and the contents of the selected
//! VM, or a placeholder while there is nothing to show.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::Frame;

use crate::cli::args::Format;
use crate::cli::output::table::Color as CellColor;
use crate::cli::output::views::VmDetailView;
use crate::cli::output::{DetailTable, RenderOptions};
use crate::tui::store::{DetailTab, State};

/// Renders the pane into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let block = Block::bordered();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [tabs_area, body_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let tabs = Tabs::new(["1 Details", "2 Checkpoints"])
        .select(state.detail.tab.index())
        .highlight_style(Style::new().reversed());
    frame.render_widget(tabs, tabs_area);
    match state.detail.tab {
        DetailTab::Checkpoints => render_checkpoints(frame, body_area, state),
        DetailTab::Details => frame.render_widget(
            Paragraph::new(body(state)).scroll((state.detail.scroll, 0)),
            body_area,
        ),
    }
}

/// The lines under the tab bar on the Details tab.
fn body(state: &State) -> Vec<Line<'static>> {
    if state.vm_list.items.is_empty() {
        return vec![Line::raw("No VMs")];
    }
    match &state.detail.detail {
        Some(detail) => detail_lines(detail),
        None => vec![Line::raw("Loading...")],
    }
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
    let list = List::new(items).highlight_style(Style::new().reversed());
    let mut list_state = ListState::default().with_selected(checkpoints.selected_index());
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// The fields of `vm get`, rendered from the very rows the CLI prints.
fn detail_lines(detail: &hypervctl_core::VmDetail) -> Vec<Line<'static>> {
    let view = VmDetailView::from_detail(detail.clone());
    let opts = RenderOptions {
        format: Format::Table,
        no_header: false,
        no_trunc: false,
        color: true,
        max_width: None,
    };
    view.detail_rows(&opts)
        .into_iter()
        .map(|(label, cell)| {
            let style = match cell.color() {
                Some(CellColor::Green) => Style::new().fg(Color::Green),
                Some(CellColor::Yellow) => Style::new().fg(Color::Yellow),
                Some(CellColor::Red) => Style::new().fg(Color::Red),
                None => Style::new(),
            };
            Line::from(vec![
                Span::raw(format!("{label:<18}")),
                Span::raw("  "),
                Span::styled(cell.text().to_string(), style),
            ])
        })
        .collect()
}
