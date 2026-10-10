//! JSON rendering: pretty printed, keys in the order of the view (design 4.4).

use serde::Serialize;

use hypervctl_core::{HyperVError, RawError};

/// Pretty JSON with a trailing newline, so that stdout ends where a reader
/// expects it to.
pub fn render<T>(value: &T) -> Result<String, HyperVError>
where
    T: ?Sized + Serialize,
{
    match serde_json::to_string_pretty(value) {
        Ok(text) => Ok(format!("{text}\n")),
        Err(error) => Err(HyperVError::backend(RawError::new(format!(
            "could not encode JSON: {error}"
        )))),
    }
}
