//! The thumbnail of the VM on screen (spec 3.10, design 5.13).

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hypervctl_core::VmId;
use ratatui_image::protocol::Protocol;
use ratatui_image::FontSize;

/// How often the thumbnail is refreshed at most (spec 3.10).
pub const THUMBNAIL_INTERVAL: Duration = Duration::from_secs(5);

/// The largest pixel size Hyper-V answers for: the request is refused only when
/// *both* sides are over (M13-V, `docs/hypervctl-m13-findings.md`).
pub const MAX_WIDTH: u16 = 1024;
pub const MAX_HEIGHT: u16 = 768;

/// A ready-to-draw thumbnail. `Protocol` is neither `Debug` nor `PartialEq`, so
/// the store wraps it: `Arc` keeps clones cheap and equality is identity.
#[derive(Clone)]
pub struct ThumbnailImage(Arc<Protocol>);

impl ThumbnailImage {
    pub fn new(protocol: Protocol) -> Self {
        Self(Arc::new(protocol))
    }

    pub fn protocol(&self) -> &Protocol {
        &self.0
    }
}

impl fmt::Debug for ThumbnailImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ThumbnailImage")
    }
}

impl PartialEq for ThumbnailImage {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// The thumbnail of the VM on screen (design 5.13).
#[derive(Debug, Default)]
pub struct ThumbnailState {
    /// Which VM the image (or the request) belongs to; a result for another VM
    /// is dropped (spec 3.10).
    pub for_vm: Option<VmId>,
    /// The size in cells the current image was built for; a resize asks again.
    pub cells: Option<(u16, u16)>,
    /// The decoded image, ready to draw.
    pub protocol: Option<ThumbnailImage>,
    /// A fetch is running; do not pile another one up (design 5.5).
    pub in_flight: bool,
    /// When the last fetch started, so the next one waits at least
    /// [`THUMBNAIL_INTERVAL`] (spec 3.10).
    pub last_request: Option<Instant>,
}

/// The pixel size to ask Hyper-V for: the pane in pixels, shrunk so that not
/// both sides are over the provider's limit (M13-V).
pub fn requested_pixels(cells: (u16, u16), font: FontSize) -> (u16, u16) {
    let width = cells.0.max(1).saturating_mul(font.width.max(1));
    let height = cells.1.max(1).saturating_mul(font.height.max(1));
    if width > MAX_WIDTH && height > MAX_HEIGHT {
        let scale = (f64::from(MAX_WIDTH) / f64::from(width))
            .min(f64::from(MAX_HEIGHT) / f64::from(height));
        let scaled_width = (f64::from(width) * scale).floor() as u16;
        let scaled_height = (f64::from(height) * scale).floor() as u16;
        return (scaled_width.max(1), scaled_height.max(1));
    }
    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_pane_is_asked_for_as_is() {
        let font = FontSize::new(10, 20);
        assert_eq!(requested_pixels((40, 12), font), (400, 240));
    }

    #[test]
    fn a_pane_over_both_limits_is_scaled_down() {
        let font = FontSize::new(10, 20);
        // 200x60 cells -> 2000x1200 pixels: both over, so it is scaled until
        // both fit.
        let (width, height) = requested_pixels((200, 60), font);
        assert!(
            width <= MAX_WIDTH && height <= MAX_HEIGHT,
            "{width}x{height}"
        );
    }

    #[test]
    fn one_side_over_the_limit_is_left_alone() {
        let font = FontSize::new(10, 20);
        // 2000 wide but only 600 tall: the height is fine, so nothing shrinks.
        assert_eq!(requested_pixels((200, 30), font), (2000, 600));
    }
}
