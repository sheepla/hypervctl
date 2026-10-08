//! In-memory backend used by tests and for TUI development on non-Windows hosts.
//!
//! Implemented by milestone M2.

#![forbid(unsafe_code)]

/// State machine standing in for Hyper-V (design 3.5).
#[derive(Debug)]
pub struct MockBackend;
