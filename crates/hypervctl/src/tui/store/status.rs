//! The status message of the status bar: what happened, how serious it is,
//! and when it stops being shown (design 5.3).

use std::time::Instant;

/// How serious a message is; it decides the colour (the words stay the same,
/// so nothing depends on colour alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Error,
}

/// One message on the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMessage {
    pub kind: StatusKind,
    pub text: String,
    /// When the message stops being shown by itself; `None` keeps it until
    /// something replaces it (spec 3.5 uses a deadline for operation results).
    pub deadline: Option<Instant>,
}

/// The current message, if any (design 5.3).
#[derive(Debug, Default)]
pub struct StatusState {
    pub current: Option<StatusMessage>,
}

impl StatusState {
    /// Replaces the current message with one that stays until replaced.
    pub fn show(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.current = Some(StatusMessage {
            kind,
            text: text.into(),
            deadline: None,
        });
    }

    /// Drops the message once its deadline has passed (the `UiTick` calls this).
    pub fn expire(&mut self, now: Instant) {
        if self
            .current
            .as_ref()
            .is_some_and(|message| message.deadline.is_some_and(|at| now >= at))
        {
            self.current = None;
        }
    }

    /// Whether a `UiTick` is needed to expire something (design 5.2).
    pub fn needs_tick(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|message| message.deadline.is_some())
    }
}
