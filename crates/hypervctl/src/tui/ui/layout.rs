//! The layout arithmetic as pure functions (design 5.11): the reducer and the
//! view both ask these, so no height rule exists twice.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Padding};

/// The gap between two list columns (spec 3.2).
const GAP: u16 = 2;
const MARKER: u16 = 2;
const STATE: u16 = 11;
const CPU: u16 = 5;
const MEMORY: u16 = 9;
const UPTIME: u16 = 11;

/// Everything right of the name column: markers, columns and their gaps.
const FIXED: u16 = MARKER + GAP + STATE + GAP + CPU + GAP + MEMORY + GAP + UPTIME;

/// The fixed columns of the checkpoints tree (spec 3.8): `production` and a
/// minute-precision local time are the widest values.
const KIND: u16 = 11;
const CREATED: u16 = 16;

/// The narrowest NAME column (spec 3.2).
const NAME_MIN: u16 = 12;

/// Rows the list needs above its items: two border rows and the header
/// (spec 3.2: `n + 3`).
const LIST_OVERHEAD: u16 = 3;

/// Rows the detail pane spends on borders, the tab bar and the blank line
/// under it (spec 3.2).
const DETAIL_OVERHEAD: u16 = 2 + 1 + 1;

/// Where the panes go (design 5.11): the list on top, the detail below it,
/// the status bar last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub list: Rect,
    pub detail: Rect,
    pub status: Rect,
}

/// The pane heights of spec 3.2: `L = clamp(n + 3, 6, max(6, floor((H-1)
/// * 0.4)))`, one row for the status bar, the rest is the detail pane.
pub fn compute(width: u16, height: u16, vm_count: usize) -> Layout {
    let status_height = 1.min(height);
    let rest = height - status_height;
    let cap = ((usize::from(rest) * 2) / 5).max(6) as u16;
    let wanted = (vm_count + 3).min(u16::MAX as usize) as u16;
    let list_height = wanted.clamp(6, cap).min(rest);
    Layout {
        list: Rect::new(0, 0, width, list_height),
        detail: Rect::new(0, list_height, width, rest - list_height),
        status: Rect::new(0, rest, width, status_height),
    }
}

/// How many lines of the Details body fit: the pane height minus the two
/// border rows, the tab row and the blank row (design 5.11).
pub fn detail_body_height(layout: Layout) -> u16 {
    layout.detail.height.saturating_sub(DETAIL_OVERHEAD)
}

/// How many VM rows fit under the header (design 5.11).
pub fn list_body_height(layout: Layout) -> u16 {
    layout.list.height.saturating_sub(LIST_OVERHEAD)
}

/// The cells the Thumbnail image has to fit: the detail pane's inner width and
/// the body under the tab bar (spec 3.10, design 5.13). The view draws into the
/// very same rectangle, so a fetched image always fits.
pub fn thumbnail_cells(layout: Layout) -> (u16, u16) {
    (
        layout.detail.width.saturating_sub(4),
        layout.detail.height.saturating_sub(3),
    )
}

/// The widths of the list columns for an inner width (spec 3.2): only NAME
/// flexes, everything else keeps the size the specification gives it.
pub fn list_columns(inner_width: u16) -> ListColumns {
    let name = inner_width.saturating_sub(FIXED).max(NAME_MIN);
    ListColumns {
        marker: MARKER,
        name,
        state: STATE,
        cpu: CPU,
        memory: MEMORY,
        uptime: UPTIME,
        gap: GAP,
    }
}

/// One list row's column widths (spec 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListColumns {
    pub marker: u16,
    pub name: u16,
    pub state: u16,
    pub cpu: u16,
    pub memory: u16,
    pub uptime: u16,
    pub gap: u16,
}

impl ListColumns {
    /// The width of a whole rendered row, header included.
    pub fn total(self) -> u16 {
        self.marker + self.name + 4 * self.gap + self.state + self.cpu + self.memory + self.uptime
    }
}

/// The widths of the checkpoints tree rows for an inner width (spec 3.8):
/// only the name column flexes, the tree prefix lives inside it.
pub fn checkpoint_columns(inner_width: u16) -> CheckpointColumns {
    let fixed = MARKER + GAP + KIND + GAP + CREATED;
    let name = inner_width.saturating_sub(fixed).max(NAME_MIN);
    CheckpointColumns {
        marker: MARKER,
        name,
        kind: KIND,
        created: CREATED,
        gap: GAP,
    }
}

/// One checkpoints tree row's column widths (spec 3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointColumns {
    pub marker: u16,
    pub name: u16,
    pub kind: u16,
    pub created: u16,
    pub gap: u16,
}

impl CheckpointColumns {
    /// The width of a whole rendered row, header included.
    pub fn total(self) -> u16 {
        self.marker + self.name + 2 * self.gap + self.kind + self.created
    }
}

/// A bordered pane: one column of padding on each side and a thick frame
/// while the pane has the focus (spec 3.2, design 5.11). `top_right` sits on
/// the top border's right end (`[mock] stale 12s`), `bottom_right` on the
/// bottom one (`rows 5-8 of 12`).
pub fn pane_block(
    title: impl Into<String>,
    top_right: Option<String>,
    bottom_right: Option<String>,
    focused: bool,
) -> Block<'static> {
    let title = title.into();
    let mut block = Block::bordered()
        .padding(Padding::horizontal(1))
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        });
    if !title.is_empty() {
        block = block.title(title);
    }
    if let Some(right) = top_right {
        block = block.title(Line::from(right).right_aligned());
    }
    // The range label lives on the bottom border (spec 3.2).
    if let Some(right) = bottom_right {
        block = block.title_bottom(Line::from(right).right_aligned());
    }
    block
}

/// The rectangle a popup may use: centred inside the detail pane, at least 28
/// columns wide, never touching the pane's frame (spec 3.4, design 5.11).
pub fn popup_rect(detail: Rect, content_width: u16, content_height: u16) -> Rect {
    let max_width = detail.width.saturating_sub(2).max(1);
    let wanted = content_width.saturating_add(4);
    let width = if max_width <= 28 {
        max_width
    } else {
        wanted.clamp(28, max_width)
    };
    let max_height = detail.height.saturating_sub(2).max(1);
    let height = content_height.saturating_add(2).min(max_height).max(1);
    centre(detail, width, height)
}

/// The help overlay's rectangle: the whole terminal, two rows and two columns
/// in from every edge (design 5.11).
pub fn help_rect(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(2),
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    )
}

/// `width` by `height`, centred inside `area` (design 5.11).
fn centre(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anomaly 23: the height table of spec 3.2 over the sizes the checklist
    /// names, with the two invariants it demands.
    #[test]
    fn the_height_table_matches_the_specification() {
        let table = [
            // (vm count, terminal height, expected list height)
            (0, 24, 6),
            (1, 24, 6),
            (4, 24, 7),
            (6, 24, 9),
            (12, 24, 9),
            (0, 30, 6),
            (1, 30, 6),
            (4, 30, 7),
            (6, 30, 9),
            // floor((30-1) * 0.4) = 11 caps the list before 15 would fit.
            (12, 30, 11),
            (0, 50, 6),
            (1, 50, 6),
            (4, 50, 7),
            (6, 50, 9),
            (12, 50, 15),
        ];
        for (count, height, expected) in table {
            let layout = compute(80, height, count);
            assert_eq!(
                layout.list.height, expected,
                "VMs {count} at height {height}"
            );
            assert_eq!(
                layout.list.height + layout.detail.height + 1,
                height,
                "the panes must fill the terminal"
            );
            assert_eq!(layout.status.height, 1);
            assert!(
                layout.list.height >= 6,
                "the list keeps six rows: {layout:?}"
            );
        }
        // At 80x24 the detail body has at least ten rows (checklist 23).
        let layout = compute(80, 24, 4);
        assert!(detail_body_height(layout) >= 10, "{layout:?}");
        assert_eq!(detail_body_height(layout), 12);
        assert_eq!(list_body_height(layout), layout.list.height - 3);
    }

    #[test]
    fn the_name_column_absorbs_the_rest_of_the_width() {
        // The worked example of spec 3.2: an inner width of 76 gives NAME 30.
        let columns = list_columns(76);
        assert_eq!(columns.name, 30);
        assert_eq!(columns.state, 11);
        assert_eq!(columns.cpu, 5);
        assert_eq!(columns.memory, 9);
        assert_eq!(columns.uptime, 11);
        assert_eq!(columns.total(), 76);
        // A tiny inner width still keeps NAME readable.
        assert_eq!(list_columns(10).name, NAME_MIN);
    }

    #[test]
    fn the_checkpoint_columns_fit_the_widest_values() {
        // spec 3.8: TYPE and CREATED are fixed, the name flexes with the tree.
        let columns = checkpoint_columns(76);
        assert_eq!(columns.marker, 2);
        assert_eq!(columns.kind, 11);
        assert_eq!(columns.created, 16);
        assert_eq!(columns.name, 76 - 33);
        assert_eq!(columns.total(), 76);
        assert_eq!(checkpoint_columns(10).name, NAME_MIN);
    }

    #[test]
    fn a_popup_stays_inside_the_detail_pane() {
        let detail = Rect::new(0, 7, 80, 16);
        let popup = popup_rect(detail, 35, 11);
        assert!(popup.x >= detail.x);
        assert!(popup.y >= detail.y);
        assert!(popup.x + popup.width <= detail.x + detail.width);
        assert!(popup.y + popup.height <= detail.y + detail.height);
        assert!(popup.width >= 28, "{popup:?}");
        assert_eq!(popup.width, 39);
        assert_eq!(popup.height, 13);
        // Wide content is still capped by the pane.
        let wide = popup_rect(detail, 100, 40);
        assert!(wide.width <= detail.width - 2);
        assert!(wide.height <= detail.height - 2);
    }

    #[test]
    fn the_help_overlay_sits_two_cells_off_every_edge() {
        let help = help_rect(Rect::new(0, 0, 80, 24));
        assert_eq!(help, Rect::new(2, 2, 76, 20));
    }

    #[test]
    fn only_the_focused_pane_gets_a_thick_frame() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;
        let area = Rect::new(0, 0, 20, 3);
        let mut focused = Buffer::empty(area);
        pane_block("VMs (4)", None, None, true).render(area, &mut focused);
        assert_eq!(focused[(0, 0)].symbol(), "┏");
        let mut other = Buffer::empty(area);
        pane_block("web-01 · Running", None, None, false).render(area, &mut other);
        assert_eq!(other[(0, 0)].symbol(), "┌");
    }
}
