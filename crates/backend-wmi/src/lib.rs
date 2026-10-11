//! WMI-backed implementation of the Hyper-V backend.
//!
//! The WMI connection itself only exists on Windows (`cfg(windows)`). The
//! HRESULT classification is pure and compiles everywhere so its tests run on
//! any CI target. On other targets this crate still links, so the whole
//! workspace builds and tests anywhere; the binary then falls back to the mock
//! backend outside Windows.

pub mod error;

#[cfg(windows)]
mod wmi;

#[cfg(windows)]
pub use wmi::WmiBackend;

pub use error::classify_hresult;
