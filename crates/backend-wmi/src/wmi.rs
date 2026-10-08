//! WMI backend implementation.
//!
//! Filled in by the backend milestones (M6: read-only operations, M7: mutations).

/// Handle to a WMI connection pool against `root\virtualization\v2`.
///
/// Created on a dedicated thread because WMI objects are not `Send` (design 3.2).
#[derive(Debug)]
pub struct WmiBackend;
