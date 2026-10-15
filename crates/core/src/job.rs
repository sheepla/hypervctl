//! Jobs and the accepted/rejected shape of a mutation.

use std::fmt;
use std::time::Duration;

use crate::ids::VmId;
use crate::vm::VmState;

/// The outcome of asking the backend to change something (design 3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Finished while the call was running.
    Done,
    /// A background job was started; poll it with `job_status`.
    Job(JobRef),
    /// No job exists. The operation is finished when the VM reaches the state.
    AwaitState(VmId, VmState),
    /// A guest reboot was requested via `InitiateReboot`. There is no job and
    /// no state transition: the VM stays `Running`. Completion is detected by
    /// the uptime resetting below `uptime_before` (design 3.3). `None` means
    /// the uptime could not be read before the request, so completion cannot
    /// be confirmed.
    AwaitReboot {
        vm: VmId,
        uptime_before: Option<Duration>,
    },
}

/// Handle to a job the backend started.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobRef(String);

impl JobRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for JobRef {
    fn from(id: &str) -> Self {
        Self(id.to_string())
    }
}

impl From<String> for JobRef {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// Progress of a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    /// `None` when the backend does not know a percentage.
    Running(Option<u32>),
    Succeeded,
    Failed {
        code: u32,
        message: String,
    },
    Cancelled,
}

impl JobStatus {
    /// `true` when polling can stop. `Running` is not finished even at 100%.
    pub const fn is_finished(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed { .. } | Self::Cancelled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_terminal_states_are_finished() {
        assert!(!JobStatus::Running(None).is_finished());
        assert!(!JobStatus::Running(Some(100)).is_finished());
        assert!(JobStatus::Succeeded.is_finished());
        assert!(JobStatus::Cancelled.is_finished());
        assert!(JobStatus::Failed {
            code: 32770,
            message: "no".to_string()
        }
        .is_finished());
    }

    #[test]
    fn job_refs_round_trip() {
        let job = JobRef::from("job-1");
        assert_eq!(job.as_str(), "job-1");
        assert_eq!(job.to_string(), "job-1");
        assert_eq!(JobRef::new(String::from("job-2")).to_string(), "job-2");
    }

    #[test]
    fn await_state_carries_the_target() {
        let pending = Pending::AwaitState(VmId::new(uuid::Uuid::nil()), VmState::Off);
        assert!(matches!(pending, Pending::AwaitState(_, VmState::Off)));
    }
}
