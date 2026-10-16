//! The availability table (spec 2.9) as a single pure function.
//!
//! Both the operation layer and the TUI menu derive their enable/disable
//! decisions from this one table. `plan()` and the TUI's `vm_item_enabled`
//! are gone; nothing else may hold a state→permission table (design 3.3).

use std::fmt;

use crate::vm::VmState;

/// One operation the user can ask for, on a VM or on a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    Start,
    Stop,
    StopForce,
    Restart,
    RestartForce,
    Save,
    Pause,
    Resume,
    CheckpointCreate,
    CheckpointRestore,
    CheckpointRemove,
}

impl OpKind {
    /// The operation behind a CLI verb / menu item. `restart` and
    /// `restart --force` share one row in the availability table.
    pub const ALL: [OpKind; 11] = [
        OpKind::Start,
        OpKind::Stop,
        OpKind::StopForce,
        OpKind::Restart,
        OpKind::RestartForce,
        OpKind::Save,
        OpKind::Pause,
        OpKind::Resume,
        OpKind::CheckpointCreate,
        OpKind::CheckpointRestore,
        OpKind::CheckpointRemove,
    ];
}

impl fmt::Display for OpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::StopForce => "stop --force",
            Self::Restart => "restart",
            Self::RestartForce => "restart --force",
            Self::Save => "save",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::CheckpointCreate => "checkpoint create",
            Self::CheckpointRestore => "checkpoint restore",
            Self::CheckpointRemove => "checkpoint remove",
        })
    }
}

/// What the table says for one (operation, state) pair (spec 2.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Availability {
    /// Run it: the VM is not already in the goal state.
    Ok,
    /// Already there: report success without touching the backend.
    Already,
    /// Transitional: wait for a stable state, then re-evaluate.
    Wait,
    /// Not valid here: `InvalidState`.
    Reject,
}

impl fmt::Display for Availability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::Already => "already",
            Self::Wait => "wait",
            Self::Reject => "reject",
        })
    }
}

/// The availability table of spec 2.9.
///
/// The cells the specification once marked `*` (unverified on real hardware)
/// follow M11-V: `stop --force`/Saved, `save`/Paused and
/// `checkpoint restore`/Saved are `Ok`; `checkpoint restore`/Paused stays
/// `Reject` (`ApplySnapshot` answers `32775`). See
/// `docs/hypervctl-m11-findings.md`.
pub fn availability(op: OpKind, state: VmState) -> Availability {
    use Availability::*;
    use OpKind::*;
    use VmState::*;

    if matches!(state, Unknown(_)) {
        return Reject;
    }
    if state.is_transition() {
        return Wait;
    }

    match (op, state) {
        // start: OK from Off/Saved/Paused, already when Running.
        (Start, Running) => Already,
        (Start, Off | Saved | Paused) => Ok,

        // stop: OK from Running, already when Off.
        (Stop, Running) => Ok,
        (Stop, Off) => Already,

        // stop --force: OK from Running, Paused and Saved, already when Off
        // (M11-V V1 confirmed TurnOff on a Saved VM).
        (StopForce, Running | Paused | Saved) => Ok,
        (StopForce, Off) => Already,

        // restart / restart --force: OK only from Running.
        (Restart | RestartForce, Running) => Ok,

        // save: OK from Running and Paused, already when Saved (M11-V V2
        // confirmed Save on a Paused VM).
        (Save, Running | Paused) => Ok,
        (Save, Saved) => Already,

        // pause: OK from Running, already when Paused.
        (Pause, Running) => Ok,
        (Pause, Paused) => Already,

        // resume: OK from Paused, already when Running.
        (Resume, Paused) => Ok,
        (Resume, Running) => Already,

        // checkpoint create / remove: allowed in every stable state.
        (CheckpointCreate | CheckpointRemove, Running | Off | Saved | Paused) => Ok,

        // checkpoint restore: OK from Off and Saved (M11-V V3 confirmed
        // ApplySnapshot succeeds from a Saved VM). Paused stays Reject: the
        // provider answers `32775` there.
        (CheckpointRestore, Off | Saved) => Ok,

        // Every other combination is Reject.
        _ => Reject,
    }
}

/// Every stable state in which `op` is `Ok`, in table order. The TUI uses
/// this to generate the "can run in …" hint for a disabled menu item without
/// duplicating the table (design 5.7).
pub fn states_where_ok(op: OpKind) -> Vec<VmState> {
    [
        VmState::Running,
        VmState::Off,
        VmState::Saved,
        VmState::Paused,
    ]
    .into_iter()
    .filter(|state| availability(op, *state) == Availability::Ok)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full table of spec 2.9, cell by cell. Any change to the table
    /// must change this test at the same time.
    #[test]
    fn the_specification_table_cell_by_cell() {
        use Availability::*;
        use OpKind::*;
        use VmState::*;

        let expected: &[(OpKind, VmState, Availability)] = &[
            // start
            (Start, Running, Already),
            (Start, Off, Ok),
            (Start, Saved, Ok),
            (Start, Paused, Ok),
            (Start, Busy, Wait),
            (Start, Unknown(42), Reject),
            // stop
            (Stop, Running, Ok),
            (Stop, Off, Already),
            (Stop, Saved, Reject),
            (Stop, Paused, Reject),
            (Stop, Busy, Wait),
            (Stop, Unknown(42), Reject),
            // stop --force (M11-V V1: Saved is Ok)
            (StopForce, Running, Ok),
            (StopForce, Off, Already),
            (StopForce, Saved, Ok),
            (StopForce, Paused, Ok),
            (StopForce, Busy, Wait),
            (StopForce, Unknown(42), Reject),
            // restart
            (Restart, Running, Ok),
            (Restart, Off, Reject),
            (Restart, Saved, Reject),
            (Restart, Paused, Reject),
            (Restart, Busy, Wait),
            (Restart, Unknown(42), Reject),
            // restart --force
            (RestartForce, Running, Ok),
            (RestartForce, Off, Reject),
            (RestartForce, Saved, Reject),
            (RestartForce, Paused, Reject),
            (RestartForce, Busy, Wait),
            (RestartForce, Unknown(42), Reject),
            // save (M11-V V2: Paused is Ok)
            (Save, Running, Ok),
            (Save, Off, Reject),
            (Save, Saved, Already),
            (Save, Paused, Ok),
            (Save, Busy, Wait),
            (Save, Unknown(42), Reject),
            // pause
            (Pause, Running, Ok),
            (Pause, Off, Reject),
            (Pause, Saved, Reject),
            (Pause, Paused, Already),
            (Pause, Busy, Wait),
            (Pause, Unknown(42), Reject),
            // resume
            (Resume, Running, Already),
            (Resume, Off, Reject),
            (Resume, Saved, Reject),
            (Resume, Paused, Ok),
            (Resume, Busy, Wait),
            (Resume, Unknown(42), Reject),
            // checkpoint create
            (CheckpointCreate, Running, Ok),
            (CheckpointCreate, Off, Ok),
            (CheckpointCreate, Saved, Ok),
            (CheckpointCreate, Paused, Ok),
            (CheckpointCreate, Busy, Wait),
            (CheckpointCreate, Unknown(42), Reject),
            // checkpoint restore (M11-V V3: Saved is Ok, Paused stays Reject)
            (CheckpointRestore, Running, Reject),
            (CheckpointRestore, Off, Ok),
            (CheckpointRestore, Saved, Ok),
            (CheckpointRestore, Paused, Reject),
            (CheckpointRestore, Busy, Wait),
            (CheckpointRestore, Unknown(42), Reject),
            // checkpoint remove
            (CheckpointRemove, Running, Ok),
            (CheckpointRemove, Off, Ok),
            (CheckpointRemove, Saved, Ok),
            (CheckpointRemove, Paused, Ok),
            (CheckpointRemove, Busy, Wait),
            (CheckpointRemove, Unknown(42), Reject),
        ];

        for &(op, state, want) in expected {
            assert_eq!(
                availability(op, state),
                want,
                "{op} in {state} should be {want}"
            );
        }
    }

    #[test]
    fn states_where_ok_lists_only_ok_states() {
        assert_eq!(
            states_where_ok(OpKind::Start),
            vec![VmState::Off, VmState::Saved, VmState::Paused]
        );
        assert_eq!(states_where_ok(OpKind::Stop), vec![VmState::Running]);
        assert_eq!(states_where_ok(OpKind::Restart), vec![VmState::Running]);
        assert!(!states_where_ok(OpKind::CheckpointRestore).is_empty());
        assert_eq!(
            states_where_ok(OpKind::CheckpointRestore),
            vec![VmState::Off, VmState::Saved]
        );
    }

    #[test]
    fn op_kind_has_stable_display_names() {
        assert_eq!(OpKind::Start.to_string(), "start");
        assert_eq!(OpKind::StopForce.to_string(), "stop --force");
        assert_eq!(OpKind::RestartForce.to_string(), "restart --force");
        assert_eq!(OpKind::CheckpointCreate.to_string(), "checkpoint create");
        assert_eq!(OpKind::CheckpointRestore.to_string(), "checkpoint restore");
        assert_eq!(OpKind::CheckpointRemove.to_string(), "checkpoint remove");
        assert_eq!(OpKind::ALL.len(), 11);
    }
}
