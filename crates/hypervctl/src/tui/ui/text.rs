//! Text fitting (design 5.11): the one path rule of spec 3.2 and padding by
//! display width, so a wide name never shifts the columns.

use crate::cli::output::table::{truncate, width as measure};

/// A path keeps its file name and loses the middle (`C:\HyperV\...\disk.vhdx`);
/// anything else is cut at the tail (spec 3.2). Display width decides, never
/// the byte count. The immediate parent folder is what the ellipsis replaces,
/// and when it fits the separator in front of the file name stays visible.
pub fn middle_ellipsis(path: &str, max: usize) -> String {
    if measure(path) <= max {
        return path.to_string();
    }
    let mut segments: Vec<(usize, &str)> = Vec::new();
    let mut start = 0;
    for (index, character) in path.char_indices() {
        if character == '/' || character == '\\' {
            segments.push((start, &path[start..index]));
            start = index + character.len_utf8();
        }
    }
    segments.push((start, &path[start..]));
    let Some(&(file_start, file)) = segments
        .iter()
        .rev()
        .find(|(_, segment)| !segment.is_empty())
    else {
        return truncate(path, max);
    };
    let file_width = measure(file);
    // The parent folder right in front of the file name, if there is one.
    let parent_start = segments
        .iter()
        .take_while(|(index, _)| *index < file_start)
        .enumerate()
        .filter(|(_, (_, segment))| !segment.is_empty())
        .map(|(_, (index, _))| *index)
        .last();
    let Some(parent_start) = parent_start else {
        // No folder to drop: keep the head before the file name.
        let head = take_width(&path[..file_start], max.saturating_sub(3 + file_width));
        let cut = format!("{head}...{file}");
        return if measure(&cut) <= max {
            cut
        } else {
            truncate(&format!("...{file}"), max)
        };
    };
    let head = &path[..parent_start];
    let separator = path[..file_start].chars().last().unwrap_or('/');
    let with_separator = format!("{head}...{separator}{file}");
    if measure(&with_separator) <= max {
        return with_separator;
    }
    let plain = format!("{head}...{file}");
    if measure(&plain) <= max {
        return plain;
    }
    // Not even the file name fits with the marker: cut the tail like any
    // other value.
    truncate(&format!("...{file}"), max)
}

/// The first `max` display columns of `text`.
pub fn take_width(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let step = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + step > max {
            break;
        }
        out.push(character);
        used += step;
    }
    out
}

/// `text` padded with spaces to `width` display columns (or cut short with a
/// trailing `…` when it is wider).
pub fn pad_right(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if measure(text) > width {
        return truncate(text, width);
    }
    format!("{}{}", text, " ".repeat(width - measure(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_keeps_its_file_name() {
        assert_eq!(
            middle_ellipsis(r"C:\HyperV\web-01\disk.vhdx", 23),
            r"C:\HyperV\...\disk.vhdx"
        );
        assert_eq!(
            middle_ellipsis("/var/lib/hyperv/disk.vhdx", 21),
            "/var/lib/...disk.vhdx"
        );
        // Short enough already: nothing changes.
        assert_eq!(
            middle_ellipsis(r"C:\HyperV\disk.vhdx", 40),
            r"C:\HyperV\disk.vhdx"
        );
        // The file name alone does not fit: the tail carries the ellipsis.
        let cut = middle_ellipsis(r"C:\HyperV\disk.vhdx", 10);
        assert!(cut.ends_with('…'), "{cut}");
        assert_eq!(measure(&cut), 10, "{cut}");
        // A path with a trailing separator still finds the file name.
        assert_eq!(
            middle_ellipsis(r"C:\HyperV\disk.vhdx\", 16),
            r"C:\...\disk.vhdx"
        );
    }

    #[test]
    fn padding_counts_columns_not_bytes() {
        assert_eq!(pad_right("web-01", 8), "web-01  ");
        assert_eq!(pad_right("<State>", 5), "<Sta…"); // wider: cut
        assert_eq!(measure(&pad_right("あい", 6)), 6);
        assert_eq!(pad_right("x", 0), "");
        assert_eq!(take_width("abcdef", 3), "abc");
    }
}
