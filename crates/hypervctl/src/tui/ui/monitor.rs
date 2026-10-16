//! The graph arithmetic of the Monitor tab as pure functions (spec 3.9,
//! design 5.12).

use std::time::{Duration, Instant};

use crate::tui::store::history::{Ring, Sample, RING_CAPACITY};

/// The right edge of the graph: the last ten minutes, or whatever the whole
/// ring spans when the refresh runs faster than 2 s and that is shorter
/// (design 5.12).
pub fn window(refresh: Duration) -> Duration {
    refresh
        .saturating_mul(RING_CAPACITY as u32)
        .min(Duration::from_secs(600))
}

/// One continuous run of samples a line is drawn through (design 5.12).
#[derive(Debug, PartialEq)]
pub struct Segment {
    pub points: Vec<(f64, f64)>,
}

/// The samples with a value, as points from the left edge of the window to
/// `now`: a missing value ends the run, so the gap stays a gap (spec 3.9).
/// Samples older than the window are dropped; `x` is the seconds since the
/// window started.
pub fn series(
    ring: &Ring,
    now: Instant,
    window: Duration,
    pick: fn(&Sample) -> Option<f64>,
) -> Vec<Segment> {
    let span = window.as_secs_f64();
    let mut segments: Vec<Segment> = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();
    for sample in ring.iter() {
        let age = now.saturating_duration_since(sample.at);
        if age > window {
            continue;
        }
        let Some(value) = pick(sample) else {
            if !current.is_empty() {
                segments.push(Segment {
                    points: std::mem::take(&mut current),
                });
            }
            continue;
        };
        current.push((span - age.as_secs_f64(), value));
    }
    if !current.is_empty() {
        segments.push(Segment { points: current });
    }
    segments
}

/// The CPU half of a sample (spec 3.9).
pub fn cpu(sample: &Sample) -> Option<f64> {
    sample.cpu_percent
}

/// The memory half of a sample, in bytes as a number the axis understands
/// (spec 3.9).
pub fn memory(sample: &Sample) -> Option<f64> {
    sample.memory_bytes.map(|bytes| bytes as f64)
}

/// The top of the memory axis: the biggest value in the window times 1.1, or
/// 1 GiB when there is none (spec 3.9, design 5.12).
pub fn memory_axis_max(ring: &Ring, now: Instant, window: Duration) -> f64 {
    let peak = ring
        .iter()
        .filter(|sample| now.saturating_duration_since(sample.at) <= window)
        .filter_map(|sample| sample.memory_bytes)
        .max()
        .unwrap_or(0);
    if peak == 0 {
        return 1024.0 * 1024.0 * 1024.0;
    }
    (peak as f64) * 1.1
}

/// The two ends of the time axis: `-10m` and `now` for the default window
/// (spec 3.9).
pub fn x_labels(window: Duration) -> [String; 2] {
    let minutes = (window.as_secs() / 60).max(1);
    [format!("-{minutes}m"), "now".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_of(offsets: &[u64], pick_value: impl Fn(usize) -> Option<f64>) -> (Ring, Instant) {
        let base = Instant::now();
        let mut ring = Ring::default();
        for (index, offset) in offsets.iter().enumerate() {
            ring.push(Sample {
                at: base + Duration::from_secs(*offset),
                cpu_percent: pick_value(index),
                memory_bytes: Some(1024),
            });
        }
        let now = base + Duration::from_secs(*offsets.last().unwrap_or(&0));
        (ring, now)
    }

    #[test]
    fn a_missing_value_splits_the_series_and_the_line_stops_there() {
        // Anomaly 31: a gap in the middle yields two runs, never one line
        // drawn through the missing samples (spec 3.9).
        let (ring, now) = ring_of(&[0, 10, 20, 30, 40], |index| {
            if index == 2 {
                None
            } else {
                Some(index as f64)
            }
        });
        let window = Duration::from_secs(600);
        let segments = series(&ring, now, window, cpu);
        assert_eq!(segments.len(), 2, "{segments:?}");
        assert_eq!(
            segments[0].points.len(),
            2,
            "the two samples before the gap"
        );
        assert_eq!(segments[1].points.len(), 2, "the two samples after it");
        // The newest sample sits at the right edge of the window.
        assert_eq!(segments[1].points.last().map(|point| point.0), Some(600.0));
    }

    #[test]
    fn samples_older_than_the_window_are_dropped() {
        let (ring, now) = ring_of(&[0, 300, 700], |_| Some(5.0));
        let window = Duration::from_secs(600);
        let segments = series(&ring, now, window, cpu);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].points.len(), 2, "the 700 s old sample is out");
        // The remaining sample is 400 s behind `now`, a third into the window.
        assert_eq!(segments[0].points[0].0, 200.0);
    }

    #[test]
    fn every_missing_value_leaves_the_series_empty_but_the_frame_whole() {
        // The WMI backend has no CPU number at all: the graph is empty, the
        // axes still have to be drawable (design 5.12).
        let base = Instant::now();
        let mut ring = Ring::default();
        for offset in [0, 10, 20] {
            ring.push(Sample {
                at: base + Duration::from_secs(offset),
                cpu_percent: None,
                memory_bytes: None,
            });
        }
        let now = base + Duration::from_secs(20);
        let window = Duration::from_secs(600);
        assert!(series(&ring, now, window, cpu).is_empty());
        assert!(series(&ring, now, window, memory).is_empty());
        assert_eq!(
            memory_axis_max(&ring, now, window),
            1024.0 * 1024.0 * 1024.0
        );
    }

    #[test]
    fn the_window_and_the_memory_axis_follow_the_rules_of_the_specification() {
        // 300 × 2 s is the usual ten minutes; a slower refresh would span
        // more, so ten minutes stays the cap.
        assert_eq!(window(Duration::from_secs(2)), Duration::from_secs(600));
        assert_eq!(window(Duration::from_secs(5)), Duration::from_secs(600));
        // A fast refresh shrinks the window to what the ring can hold.
        assert_eq!(window(Duration::from_millis(500)), Duration::from_secs(150));
        // The axis tops out at 1.1 × the peak, or 1 GiB without one.
        let base = Instant::now();
        let now = base + Duration::from_secs(10);
        let mut with_memory = Ring::default();
        with_memory.push(Sample {
            at: base,
            cpu_percent: None,
            memory_bytes: Some(1024 * 1024 * 1024),
        });
        let expected = 1024.0 * 1024.0 * 1024.0 * 1.1;
        assert_eq!(
            memory_axis_max(&with_memory, now, Duration::from_secs(600)),
            expected
        );
        assert_eq!(x_labels(Duration::from_secs(600)), ["-10m", "now"]);
        assert_eq!(x_labels(Duration::from_secs(150)), ["-2m", "now"]);
    }
}
