//! Object identifiers.
//!
//! Each identifier is a distinct newtype over [`Uuid`] so that a VM id can never
//! be passed where a checkpoint id is expected. Mixing them is a compile error:
//!
//! ```compile_fail
//! use hypervctl_core::{CheckpointId, VmId};
//!
//! fn needs_checkpoint(id: CheckpointId) { let _ = id; }
//!
//! fn wrong(vm: VmId) { needs_checkpoint(vm); }
//! ```
//!
//! The same identifier in the right place compiles:
//!
//! ```
//! use hypervctl_core::{CheckpointId, VmId};
//!
//! fn needs_checkpoint(id: CheckpointId) { let _ = id; }
//! fn needs_vm(id: VmId) { let _ = id; }
//!
//! fn right(vm: VmId, checkpoint: CheckpointId) {
//!     needs_vm(vm);
//!     needs_checkpoint(checkpoint);
//! }
//! ```

use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            pub const fn new(id: Uuid) -> Self {
                Self(id)
            }

            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            pub const fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Uuid {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

define_id!(
    /// Identifies a VM.
    VmId
);
define_id!(
    /// Identifies a checkpoint.
    CheckpointId
);
define_id!(
    /// Identifies a virtual switch.
    SwitchId
);

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "24d18f2f-114e-4654-9d3d-cc03106b4c21";

    #[test]
    fn round_trips_through_display_and_parse() {
        let id: VmId = SAMPLE.parse().expect("valid uuid");
        assert_eq!(id.to_string(), SAMPLE);
        let parsed: VmId = id.to_string().parse().expect("valid uuid");
        assert_eq!(parsed, id);
    }

    #[test]
    fn rejects_invalid_text() {
        assert!("not-a-uuid".parse::<VmId>().is_err());
    }

    #[test]
    fn converts_to_and_from_uuid() {
        let id = VmId::new(Uuid::nil());
        let uuid = Uuid::from(id);
        assert_eq!(uuid, Uuid::nil());
        assert_eq!(VmId::from(uuid), id);
        assert_eq!(id.as_uuid(), &uuid);
        assert_eq!(id.into_uuid(), uuid);
    }
}
