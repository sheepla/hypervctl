//! Domain model for hypervctl: identifiers, VM/checkpoint/switch/VHD types and
//! the backend-agnostic error classification.
//!
//! This crate holds pure data and pure functions only. It must build and test on
//! any target, so it depends on neither Windows nor the backend crates.

#![forbid(unsafe_code)]

pub mod actor;
pub mod checkpoint;
pub mod error;
pub mod events;
pub mod handle;
pub mod hyper_v;
pub mod ids;
pub mod job;
pub mod ops;
pub mod switch;
pub mod target;
pub mod vhd;
pub mod vm;

pub use actor::{stopped_error, BackendActor};
pub use checkpoint::{Checkpoint, CheckpointKind};
pub use error::{HyperVError, RawError, UnavailableReason};
pub use events::{ChangeEvent, ChangeWatcher, DEBOUNCE, RECONNECT_DELAY};
pub use handle::BackendHandle;
pub use hyper_v::{HyperV, RequestedState};
pub use ids::{CheckpointId, SwitchId, VmId};
pub use job::{JobRef, JobStatus, Pending};
pub use ops::{OpId, OpOutcome, OpSuccess, Operations, Phase, Progress, VmOp, WaitPolicy};
pub use switch::{SwitchDetail, SwitchKind, SwitchSummary, VlanMode, VlanSettings};
pub use target::{Candidate, Target, TargetKind};
pub use vhd::{
    AttachedVhd, Controller, ControllerKind, VhdAttachment, VhdFormat, VhdInfo, VhdKind,
};
pub use vm::{
    IntegrationService, IntegrationStatus, MemoryConfig, NicConfig, VmDetail, VmState, VmSummary,
};
