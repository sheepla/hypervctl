//! The detail pane (spec 3.2): the tab bar and the contents of the selected
//! VM, or a placeholder while there is nothing to show.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Tabs};
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
    frame.render_widget(
        Paragraph::new(body(state)).scroll((state.detail.scroll, 0)),
        body_area,
    );
}

/// The lines under the tab bar.
fn body(state: &State) -> Vec<Line<'static>> {
    if state.vm_list.items.is_empty() {
        return vec![Line::raw("No VMs")];
    }
    match state.detail.tab {
        DetailTab::Checkpoints => Vec::new(),
        DetailTab::Details => match &state.detail.detail {
            Some(detail) => detail_lines(detail),
            None => vec![Line::raw("Loading...")],
        },
    }
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
