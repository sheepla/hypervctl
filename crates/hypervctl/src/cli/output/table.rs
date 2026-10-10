//! Grid rendering: display width instead of byte length, alignment, the one
//! truncation rule and colour (design 4.4).

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::RenderOptions;

/// The only three colours the table uses; a state stays readable without them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Red,
    Green,
    Yellow,
}

impl Color {
    const fn ansi(self) -> &'static str {
        match self {
            Self::Red => "\x1b[31m",
            Self::Green => "\x1b[32m",
            Self::Yellow => "\x1b[33m",
        }
    }
}

const RESET: &str = "\x1b[0m";

/// One cell: the text as it should read, and the colour to paint it in when
/// colour is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    text: String,
    color: Option<Color>,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
        }
    }

    pub fn painted(text: impl Into<String>, color: Color) -> Self {
        Self {
            text: text.into(),
            color: Some(color),
        }
    }

    /// A value, or `-` where there is none (design 4.4).
    pub fn dash(value: Option<impl Into<String>>) -> Self {
        match value {
            Some(value) => Self::new(value),
            None => Self::new("-"),
        }
    }

    fn width(&self) -> usize {
        width(&self.text)
    }

    /// The text of the cell, for the frontends that render it themselves.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The colour the cell asks for, if any.
    pub fn color(&self) -> Option<Color> {
        self.color
    }

    fn write(&self, out: &mut String, text: &str, pad: usize, paint: bool) {
        if paint {
            if let Some(color) = self.color {
                out.push_str(color.ansi());
                out.push_str(text);
                out.push_str(RESET);
            } else {
                out.push_str(text);
            }
        } else {
            out.push_str(text);
        }
        for _ in 0..pad {
            out.push(' ');
        }
    }
}

/// The display width of `text`: a wide character counts as two columns.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Cuts `text` down to `max` display columns, ending in `…`. The cut happens
/// between characters, never inside one (design 4.4).
pub fn truncate(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    // Leave room for the ellipsis itself.
    let budget = max.saturating_sub(1);
    for character in text.chars() {
        let step = character.width().unwrap_or(0);
        if used + step > budget {
            break;
        }
        out.push(character);
        used += step;
    }
    out.push('…');
    out
}

/// Renders a grid: the header (unless `--no-header`), columns aligned by
/// display width, and only the first column ever cut short (design 4.4).
pub fn grid(headers: &[&str], rows: &[Vec<Cell>], opts: &RenderOptions) -> String {
    let columns = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut widths = vec![0usize; columns];
    if !opts.no_header {
        for (index, header) in headers.iter().enumerate() {
            widths[index] = widths[index].max(width(header));
        }
    }
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.width());
        }
    }
    // The name column is the only one that may be shortened (design 4.4).
    if let Some(max_width) = opts.max_width {
        let rest = widths.iter().skip(1).sum::<usize>() + 2 * widths.len().saturating_sub(1);
        if let Some(first) = widths.first_mut() {
            *first = (*first).min(max_width.saturating_sub(rest).max(1));
        }
    }

    let mut out = String::new();
    if !opts.no_header && !headers.is_empty() {
        let cells: Vec<Cell> = headers.iter().map(|header| Cell::new(*header)).collect();
        write_line(&mut out, &cells, &widths, opts.color);
    }
    for row in rows {
        write_line(&mut out, row, &widths, opts.color);
    }
    out
}

fn write_line(out: &mut String, row: &[Cell], widths: &[usize], paint: bool) {
    let mut line = String::new();
    for (index, cell) in row.iter().enumerate() {
        if index > 0 {
            line.push_str("  ");
        }
        let column = match widths.get(index) {
            Some(column) => *column,
            None => cell.width(),
        };
        // Only the first column is ever cut; the others are already measured.
        let text = if index == 0 {
            truncate(&cell.text, column)
        } else {
            cell.text.clone()
        };
        let pad = column.saturating_sub(width(&text));
        cell.write(&mut line, &text, pad, paint);
    }
    out.push_str(line.trim_end());
    out.push('\n');
}

/// Renders the two-column key/value view every `get` command uses (design 4.4).
pub fn key_value(rows: &[(String, Cell)], opts: &RenderOptions) -> String {
    let label_width = rows
        .iter()
        .map(|(label, _)| width(label))
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (label, cell) in rows {
        let mut line = String::new();
        line.push_str(label);
        for _ in 0..label_width.saturating_sub(width(label)) {
            line.push(' ');
        }
        line.push_str("  ");
        cell.write(&mut line, &cell.text, 0, opts.color);
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> RenderOptions {
        RenderOptions {
            format: crate::cli::args::Format::Table,
            no_header: false,
            no_trunc: false,
            color: false,
            max_width: None,
        }
    }

    #[test]
    fn wide_characters_take_two_columns() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本語"), 6);
        assert_eq!(width(""), 0);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("日本語のVM", 5), "日本…");
        assert_eq!(truncate("日本語", 6), "日本語");
        assert_eq!(truncate("ab", 1), "…");
        assert_eq!(truncate("ab", 0), "");
        assert_eq!(width(&truncate("日本語のVM", 5)), 5);
    }

    #[test]
    fn columns_are_aligned_by_display_width() {
        let rows = vec![
            vec![Cell::new("日本語"), Cell::new("1")],
            vec![Cell::new("ab"), Cell::new("2")],
        ];
        let out = grid(&["NAME", "N"], &rows, &options());
        assert_eq!(out, "NAME    N\n日本語  1\nab      2\n");
        let widths: Vec<usize> = out.lines().map(width).collect();
        assert_eq!(widths, vec![9, 9, 9]);
    }

    #[test]
    fn the_header_can_be_left_out() {
        let mut opts = options();
        opts.no_header = true;
        let out = grid(&["NAME"], &[vec![Cell::new("web-01")]], &opts);
        assert_eq!(out, "web-01\n");
    }

    #[test]
    fn colour_is_only_painted_when_it_is_asked_for() {
        let row = vec![Cell::painted("Running", Color::Green), Cell::new("x")];
        let mut opts = options();
        let plain = grid(&["STATE", "X"], std::slice::from_ref(&row), &opts);
        assert!(!plain.contains('\x1b'), "{plain:?}");
        opts.color = true;
        let painted = grid(&["STATE", "X"], &[row], &opts);
        assert!(painted.contains("\x1b[32mRunning\x1b[0m"), "{painted:?}");
        assert!(painted.ends_with("x\n"), "{painted:?}");
    }

    #[test]
    fn the_name_column_is_the_only_one_that_is_cut() {
        let mut opts = options();
        opts.max_width = Some(11);
        let rows = vec![
            vec![Cell::new("long-name"), Cell::new("1")],
            vec![Cell::new("a"), Cell::new("2")],
        ];
        let out = grid(&["NAME", "N"], &rows, &opts);
        for line in out.lines() {
            assert!(width(line) <= 11, "{line:?} is {} wide", width(line));
        }
        assert!(out.contains('…'), "{out:?}");
        assert!(!out.contains("long-name"), "{out:?}");
    }

    #[test]
    fn key_value_rows_share_a_label_column() {
        let rows = vec![
            ("Name".to_string(), Cell::new("web-01")),
            ("Checkpoint count".to_string(), Cell::new("2")),
            ("Notes".to_string(), Cell::dash(None::<String>)),
        ];
        let out = key_value(&rows, &options());
        assert_eq!(
            out,
            "Name              web-01\nCheckpoint count  2\nNotes             -\n"
        );
    }
}
