//! WMI-backed implementation of the Hyper-V backend.
//!
//! The implementation only exists on Windows (`cfg(windows)`). On other targets
//! this crate compiles to an empty crate so that the whole workspace builds and
//! tests anywhere; the binary then falls back to the mock backend.

#[cfg(windows)]
mod wmi;

#[cfg(windows)]
pub use wmi::WmiBackend;
