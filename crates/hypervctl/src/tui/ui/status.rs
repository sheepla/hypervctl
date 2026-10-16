//! The status bar as one pure function (design 5.11, spec 3.2): the hints of
//! the focused context on the left, the single message on the right, and the
//! two rules that keep both readable at 80 columns.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::cli::output::table::{truncate, width as measure};
use crate::tui::ui::text::take_width;

/// The message half of the bar: what happened and how serious it is (spec 3.5
/// and 3.2; the words carry the meaning, the colour only reinforces it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// The list on screen is behind: always prefixed with `! ` (spec 3.2).
    Stale(String),
    Error(String),
    Info(String),
}

impl Message {
    fn text(&self) -> String {
        match self {
            Self::Stale(text) => format!("! {text}"),
            Self::Error(text) | Self::Info(text) => text.clone(),
        }
    }

    fn style(&self) -> Style {
        match self {
            Self::Stale(_) => Style::new().fg(Color::Yellow),
            Self::Error(_) => Style::new().fg(Color::Red),
            Self::Info(_) => Style::new(),
        }
    }
}

/// The message may take at most 60% of the bar (spec 3.2).
const MESSAGE_PERCENT: usize = 60;

/// One row of the bar (spec 3.2): hints left, message right. A hint that does
/// not fit whole is dropped instead of cut, and `? help` is kept whatever it
/// costs.
pub fn compose(width: u16, hints: &[String], message: Option<&Message>) -> Line<'static> {
    let total = usize::from(width);
    let message = message.as_ref().map(|message| {
        let text = message.text();
        let cap = total * MESSAGE_PERCENT / 100;
        (truncate(&text, cap), message.style())
    });
    let message_width = message.as_ref().map(|(text, _)| measure(text)).unwrap_or(0);
    let available = total.saturating_sub(message_width);

    let help_index = hints.iter().position(|hint| hint == "? help");
    let help_width = help_index.map(|index| measure(&hints[index])).unwrap_or(0);
    // Two separators are reserved for the slot `? help` takes among the
    // others: it may land in the middle, which costs a space on both sides.
    let separator = if help_index.is_some() { 2 } else { 0 };
    let rest_budget = available
        .saturating_sub(help_width)
        .saturating_sub(separator);

    // Keep the hints in table order, dropping those that do not fit whole.
    let mut kept: Vec<usize> = Vec::new();
    for index in 0..hints.len() {
        if help_index == Some(index) {
            continue;
        }
        let mut trial = kept.clone();
        trial.push(index);
        if joined_width(hints, &trial) <= rest_budget {
            kept = trial;
        }
    }
    if let Some(help) = help_index {
        kept.push(help);
    }
    kept.sort_unstable();

    let mut spans = Vec::new();
    if !kept.is_empty() {
        let text = kept
            .iter()
            .map(|&index| hints[index].as_str())
            .collect::<Vec<_>>()
            .join("  ");
        spans.push(Span::styled(text, Style::new().dim()));
    }
    if let Some((text, style)) = message {
        let pad = total.saturating_sub(joined_width(hints, &kept) + measure(&text));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// The width of `indexes` joined with two spaces between them.
fn joined_width(hints: &[String], indexes: &[usize]) -> usize {
    let mut total = 0;
    for (position, &index) in indexes.iter().enumerate() {
        if position > 0 {
            total += 2;
        }
        total += measure(&hints[index]);
    }
    total
}

/// One row of the bar while the filter input is up (spec 3.2): the prompt
/// `/text_` takes the place of the key hints on the left; the message keeps
/// its right-hand slot and its 60% cap. The prompt never disappears whole: it
/// is cut at the tail instead (spec 3.3 needs the cursor line visible).
pub fn compose_filter(width: u16, text: &str, message: Option<&Message>) -> Line<'static> {
    let total = usize::from(width);
    let message = message.as_ref().map(|message| {
        let text = message.text();
        let cap = total * MESSAGE_PERCENT / 100;
        (truncate(&text, cap), message.style())
    });
    let message_width = message.as_ref().map(|(text, _)| measure(text)).unwrap_or(0);
    // The slash and the cursor always stay, whatever has to be cut (spec 3.3).
    let text_budget = total.saturating_sub(message_width + 2);
    let prompt = format!("/{}_", take_width(text, text_budget));
    let prompt_width = measure(&prompt);
    let mut spans = vec![Span::styled(prompt, Style::new().dim())];
    if let Some((text, style)) = message {
        let pad = total.saturating_sub(prompt_width + message_width);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn rendered(line: Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// Anomaly 24: at 80 columns the message is always visible, `? help`
    /// stays, and no hint is cut in half.
    #[test]
    fn the_message_is_visible_and_the_hints_are_whole_or_gone() {
        let all = hints(&[
            "q/Ctrl+C quit",
            "Tab focus",
            "r refresh",
            "Space menu",
            "? help",
            "1 details tab",
            "2 checkpoints tab",
            "j/Down move",
            "k/Up move",
            "g first",
            "G last",
            "Enter detail pane",
        ]);
        let message = Message::Info("web-01: stopped".to_string());
        let text = rendered(compose(80, &all, Some(&message)));
        assert!(text.contains("web-01: stopped"), "{text}");
        assert!(text.contains("? help"), "{text}");
        assert!(text.ends_with("web-01: stopped"), "{text}");
        // The message is right-aligned: it starts where its own width begins.
        let start = text.find("web-01").unwrap_or(text.len());
        assert_eq!(start + "web-01: stopped".len(), 80, "{text}");
        // Every hint shown is whole: the kept ones are a prefix of the table
        // around `? help`, joined with two spaces only.
        let left = text.trim_end_matches("web-01: stopped").trim_end();
        assert!(left.contains("? help"), "{text}");
        assert!(!left.contains("  ?  "), "{text}");
    }

    #[test]
    fn a_long_message_is_cut_at_the_tail_not_at_the_front() {
        let message = Message::Error("E".repeat(200));
        let text = rendered(compose(80, &hints(&["? help"]), Some(&message)));
        assert!(text.starts_with("? help"), "{text}");
        assert!(text.contains('…'), "{text}");
        // 60% of 80 columns.
        let shown: String = text.chars().skip_while(|&c| c != 'E').collect();
        assert_eq!(measure(shown.trim_end()), 48, "{text}");
    }

    #[test]
    fn help_survives_and_the_order_follows_the_table() {
        let all = hints(&["q/Ctrl+C quit", "? help", "Tab focus"]);
        let message = Message::Stale("refresh failed".to_string());
        let text = rendered(compose(40, &all, Some(&message)));
        assert!(text.contains("? help"), "{text}");
        assert!(text.contains("! refresh failed"), "{text}");
        let help = text.find("? help").unwrap_or(0);
        let quit = text.find("q/Ctrl+C").unwrap_or(usize::MAX);
        if quit != usize::MAX {
            assert!(quit < help, "{text}");
        }
    }

    #[test]
    fn without_a_message_the_hints_run_from_the_left() {
        let text = rendered(compose(80, &hints(&["q/Ctrl+C quit", "? help"]), None));
        assert_eq!(text, "q/Ctrl+C quit  ? help");
    }

    /// Spec 3.3: while the filter input is up, the left half shows `/text_`
    /// instead of the key hints, and the message keeps its slot.
    #[test]
    fn the_filter_prompt_replaces_the_hints() {
        let text = rendered(compose_filter(80, "web", None));
        assert_eq!(text, "/web_");
        let message = Message::Info("web-01: stopped".to_string());
        let text = rendered(compose_filter(80, "web-01", Some(&message)));
        assert!(text.starts_with("/web-01_"), "{text}");
        assert!(text.ends_with("web-01: stopped"), "{text}");
        // The message is still right-aligned to the last column.
        let start = text.find("web-01: stopped").unwrap_or(text.len());
        assert_eq!(start + "web-01: stopped".len(), 80, "{text}");
        // A very long input is cut at the tail rather than hiding the message.
        let text = rendered(compose_filter(80, &"x".repeat(200), Some(&message)));
        assert!(text.ends_with("web-01: stopped"), "{text}");
        let at = text.find("web-01: stopped").unwrap_or(text.len());
        let prompt = &text[..at];
        assert!(prompt.starts_with("/xxx"), "{prompt}");
        assert!(prompt.ends_with('_'), "{prompt}");
    }
}
