//! Checkpoint (snapshot) types.

use std::time::SystemTime;

use crate::ids::CheckpointId;

/// A checkpoint of one VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub name: String,
    pub kind: CheckpointKind,
    /// `None` when the provider did not report a usable timestamp.
    pub created: Option<SystemTime>,
    pub parent_id: Option<CheckpointId>,
}

/// Kind of a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckpointKind {
    Standard,
    Production,
    /// Value the backend did not map; kept so it can be shown instead of dropped.
    /// Hyper-V's "production only" checkpoints are reported as [`Self::Production`].
    Unknown(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_creation_time_stays_none() {
        let checkpoint = Checkpoint {
            id: CheckpointId::new(uuid::Uuid::nil()),
            name: "before-upgrade".to_string(),
            kind: CheckpointKind::Standard,
            created: None,
            parent_id: None,
        };
        assert_eq!(checkpoint.created, None);
        assert_eq!(checkpoint.parent_id, None);
    }

    #[test]
    fn unknown_kind_keeps_its_raw_value() {
        assert_eq!(CheckpointKind::Unknown(2), CheckpointKind::Unknown(2));
        assert_ne!(CheckpointKind::Unknown(2), CheckpointKind::Standard);
    }
}
