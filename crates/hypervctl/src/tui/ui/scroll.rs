//! Scrolling arithmetic (design 5.11): which rows are on screen and how far
//! the scroll may move. Shared by the list, the detail body and the help.

/// The visible window of `total` rows for a selection at `selected`, moved as
/// far as needed to keep it inside (spec 3.2: the selection always stays
/// visible). `None` selects nothing, so the offset is only clamped.
pub fn visible_range(selected: Option<usize>, offset: usize, height: u16, total: usize) -> usize {
    let height = usize::from(height);
    if height == 0 || total == 0 || total <= height {
        return 0;
    }
    let mut offset = offset.min(total - height);
    if let Some(selected) = selected {
        if selected < offset {
            offset = selected;
        } else if selected >= offset + height {
            offset = selected + 1 - height;
        }
    }
    offset
}

/// The same clamp the scroll actions use: `0..=(rows - height)`, so the last
/// blank line past the content can never be scrolled to (spec 3.2).
pub fn clamp(offset: u16, rows: u16, height: u16) -> u16 {
    offset.min(rows.saturating_sub(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anomaly 25: the scroll never reaches past the last row.
    #[test]
    fn the_scroll_stops_at_the_last_visible_row() {
        assert_eq!(clamp(0, 17, 12), 0);
        assert_eq!(clamp(5, 17, 12), 5);
        assert_eq!(clamp(6, 17, 12), 5);
        assert_eq!(clamp(99, 17, 12), 5);
        // Fewer rows than the height: no scrolling at all.
        assert_eq!(clamp(9, 5, 12), 0);
        assert_eq!(clamp(9, 0, 0), 0);
    }

    #[test]
    fn the_selection_always_lands_in_the_window() {
        // 12 VMs, six rows visible (the 80x24 case).
        assert_eq!(visible_range(Some(0), 0, 6, 12), 0);
        assert_eq!(visible_range(Some(3), 0, 6, 12), 0);
        assert_eq!(visible_range(Some(6), 0, 6, 12), 1);
        assert_eq!(visible_range(Some(11), 0, 6, 12), 6);
        // Selecting upwards scrolls back.
        assert_eq!(visible_range(Some(1), 6, 6, 12), 1);
        // Inside the window: the offset stays put.
        assert_eq!(visible_range(Some(4), 3, 6, 12), 3);
        // Nothing selected, too many rows: clamped only.
        assert_eq!(visible_range(None, 99, 6, 12), 6);
        // Everything fits: one window from the top.
        assert_eq!(visible_range(Some(4), 3, 6, 6), 0);
        assert_eq!(visible_range(Some(0), 0, 6, 0), 0);
    }
}
