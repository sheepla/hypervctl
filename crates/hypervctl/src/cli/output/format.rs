//! Value formatting shared by all three renderers (design 4.4).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local, SecondsFormat, Utc};

/// The units `table` prints sizes in, in steps of 1024 (design 4.4).
const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];

/// `4.0 GB` for the table; JSON and CSV keep the byte count.
///
/// Sizes beyond a gigabyte stay in gigabytes: the design lists no larger unit.
pub fn size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `[Nd ]HH:MM:SS` (design 4.4).
pub fn uptime(duration: Duration) -> String {
    let total = duration.as_secs();
    let days = total / 86_400;
    let rest = total % 86_400;
    let (hours, minutes, seconds) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    if days > 0 {
        format!("{days}d {hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    }
}

/// `3.5%`, `12%`, and `-` when there is no usable number (design 4.4).
pub fn percent(value: f64) -> String {
    if !value.is_finite() {
        return "-".to_string();
    }
    let text = format!("{value:.1}");
    match text.strip_suffix(".0") {
        Some(whole) => format!("{whole}%"),
        None => format!("{text}%"),
    }
}

/// ISO 8601 in UTC for JSON and CSV; `None` when there is no timestamp.
pub fn iso(when: Option<SystemTime>) -> Option<String> {
    let time = timestamp(when)?;
    Some(time.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// Local wall clock for `table`, `-` when there is no timestamp.
pub fn local_time(when: Option<SystemTime>) -> String {
    match timestamp(when) {
        Some(time) => time
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        None => "-".to_string(),
    }
}

/// Local wall clock without seconds for the checkpoints tab (spec 3.8):
/// `2026-10-12 09:30`, `-` when there is no timestamp.
pub fn local_minutes(when: Option<SystemTime>) -> String {
    match timestamp(when) {
        Some(time) => time
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => "-".to_string(),
    }
}

fn timestamp(when: Option<SystemTime>) -> Option<DateTime<Utc>> {
    let since_epoch = when?.duration_since(UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(since_epoch.as_secs()).ok()?;
    DateTime::from_timestamp(seconds, since_epoch.subsec_nanos())
}

/// The first eight characters of an identifier, unless `--no-trunc`
/// (spec 2.10).
pub fn short_id(id: &str, full: bool) -> String {
    if full {
        return id.to_string();
    }
    id.get(..8).unwrap_or(id).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_step_through_the_units_of_the_design() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KB");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(4 * 1024 * 1024 * 1024), "4.0 GB");
        assert_eq!(size(64 * 1024 * 1024 * 1024), "64.0 GB");
        // A day beyond a gigabyte: the largest unit of the design.
        assert_eq!(size(1024 * 1024 * 1024 * 1024), "1024.0 GB");
    }

    #[test]
    fn uptime_counts_days_only_when_there_are_any() {
        assert_eq!(uptime(Duration::ZERO), "00:00:00");
        assert_eq!(uptime(Duration::from_secs(3600)), "01:00:00");
        assert_eq!(uptime(Duration::from_secs(59)), "00:00:59");
        assert_eq!(uptime(Duration::from_secs(90_061)), "1d 01:01:01");
    }

    #[test]
    fn percent_drops_the_pointless_decimal() {
        assert_eq!(percent(3.5), "3.5%");
        assert_eq!(percent(12.0), "12%");
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(f64::NAN), "-");
        assert_eq!(percent(f64::INFINITY), "-");
    }

    #[test]
    fn utc_timestamps_are_iso_8601() {
        assert_eq!(iso(None), None);
        assert_eq!(
            iso(Some(UNIX_EPOCH)),
            Some("1970-01-01T00:00:00Z".to_string())
        );
        assert_eq!(
            iso(Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))),
            Some("2023-11-14T22:13:20Z".to_string())
        );
    }

    #[test]
    fn local_timestamps_are_readable_and_marked_when_missing() {
        assert_eq!(local_time(None), "-");
        let shown = local_time(Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
        assert!(
            chrono::NaiveDateTime::parse_from_str(&shown, "%Y-%m-%d %H:%M:%S").is_ok(),
            "not a plain local timestamp: {shown}"
        );
    }

    #[test]
    fn checkpoint_times_drop_the_seconds() {
        assert_eq!(local_minutes(None), "-");
        let shown = local_minutes(Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
        assert!(
            chrono::NaiveDateTime::parse_from_str(&shown, "%Y-%m-%d %H:%M").is_ok(),
            "not a minute-precision local timestamp: {shown}"
        );
    }

    #[test]
    fn identifiers_are_cut_on_a_character_boundary() {
        let id = "00000001-0000-0000-0000-000000000000";
        assert_eq!(short_id(id, false), "00000001");
        assert_eq!(short_id(id, true), id);
        assert_eq!(short_id("abc", false), "abc");
        assert_eq!(short_id("", false), "");
    }
}
