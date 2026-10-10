//! Progress on stderr: a spinner where there is a terminal, one line per phase
//! where there is not (design 4.5).

use std::io::{IsTerminal, Write};

use hypervctl_core::{Phase, Progress, VmOp};

/// The frames of the spinner, drawn one per progress report.
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

/// How an operation waiting for a state describes that wait (design 4.5).
pub fn waiting_label(op: VmOp) -> &'static str {
    match op {
        VmOp::Start => "starting",
        VmOp::Stop => "waiting for shutdown",
        VmOp::StopForce => "powering off",
        VmOp::Restart => "restarting",
        VmOp::RestartForce => "resetting",
        VmOp::Save => "saving",
        VmOp::Pause => "pausing",
        VmOp::Resume => "resuming",
    }
}

/// Reports the progress of one command on stderr (design 4.5). A terminal keeps
/// a single spinner line up to date; anything else gets one line each time the
/// phase changes, so a log file stays readable.
pub struct ProgressReporter<W> {
    out: W,
    tty: bool,
    frame: usize,
    /// What is on screen now, so that a pipe is not flooded with repeats.
    drawn: Option<(String, Phase)>,
    /// Whether a spinner line has to be erased before anything else is printed.
    live: bool,
}

impl ProgressReporter<std::io::Stderr> {
    /// A reporter on the real stderr, spinner if there is a terminal (design 4.5).
    pub fn new() -> Self {
        Self::with(std::io::stderr(), std::io::stderr().is_terminal())
    }
}

impl<W> ProgressReporter<W>
where
    W: Write,
{
    /// A reporter on any sink, for the tests.
    pub fn with(out: W, tty: bool) -> Self {
        Self {
            out,
            tty,
            frame: 0,
            drawn: None,
            live: false,
        }
    }

    /// Draws one progress report. `waiting` words the wait for this verb, which
    /// is what the spinner shows while a state is on its way.
    pub fn report(&mut self, progress: &Progress, waiting: &'static str) {
        let label = match progress.phase {
            Phase::Resolving => "resolving",
            Phase::Requesting => "requesting",
            Phase::Waiting => waiting,
            // The result line says the same thing, and says it better.
            Phase::Finished => {
                self.clear();
                return;
            }
        };
        let percent = match progress.percent {
            Some(percent) => format!(" {percent}%"),
            None => String::new(),
        };
        if self.tty {
            let frame = SPINNER[self.frame % SPINNER.len()];
            self.frame += 1;
            let _ = write!(
                self.out,
                "\r\x1b[2K{frame} {} {label}{percent}",
                progress.target
            );
            self.live = true;
        } else {
            let key = (progress.target.clone(), progress.phase);
            if self.drawn.as_ref() == Some(&key) {
                return;
            }
            self.drawn = Some(key);
            let _ = writeln!(self.out, "{}: {label}{percent}", progress.target);
        }
    }

    /// Erases the spinner line, so the results that follow start clean.
    pub fn clear(&mut self) {
        if self.live {
            let _ = write!(self.out, "\r\x1b[2K");
            self.live = false;
        }
    }

    /// The sink, for the tests that read what was written.
    #[cfg(test)]
    pub fn into_inner(self) -> W {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(target: &str, phase: Phase, percent: Option<u8>) -> Progress {
        Progress {
            target: target.to_string(),
            phase,
            percent,
        }
    }

    fn text(reporter: ProgressReporter<Vec<u8>>) -> String {
        String::from_utf8_lossy(&reporter.into_inner()).into_owned()
    }

    #[test]
    fn a_pipe_gets_one_line_per_phase_change() {
        let mut reporter = ProgressReporter::with(Vec::new(), false);
        reporter.report(&progress("web-01", Phase::Resolving, None), "starting");
        reporter.report(&progress("web-01", Phase::Requesting, None), "starting");
        reporter.report(&progress("web-01", Phase::Waiting, None), "starting");
        reporter.report(&progress("web-01", Phase::Waiting, Some(45)), "starting");
        reporter.report(&progress("web-01", Phase::Waiting, Some(90)), "starting");
        reporter.report(&progress("web-01", Phase::Finished, None), "starting");
        assert_eq!(
            text(reporter),
            "web-01: resolving\nweb-01: requesting\nweb-01: starting\n"
        );
    }

    #[test]
    fn a_terminal_keeps_one_spinning_line_and_erases_it_at_the_end() {
        let mut reporter = ProgressReporter::with(Vec::new(), true);
        reporter.report(&progress("web-01", Phase::Resolving, None), "starting");
        reporter.report(&progress("web-01", Phase::Waiting, Some(45)), "starting");
        reporter.report(&progress("web-01", Phase::Finished, None), "starting");
        let drawn = text(reporter);
        assert!(drawn.contains("| web-01 resolving"), "{drawn:?}");
        assert!(drawn.contains("/ web-01 starting 45%"), "{drawn:?}");
        assert!(drawn.ends_with("\r\x1b[2K"), "{drawn:?}");
        assert!(!drawn.contains('\n'), "{drawn:?}");
    }

    #[test]
    fn the_wait_of_every_verb_has_a_word() {
        assert_eq!(waiting_label(VmOp::Start), "starting");
        assert_eq!(waiting_label(VmOp::Stop), "waiting for shutdown");
        assert_eq!(waiting_label(VmOp::StopForce), "powering off");
        assert_eq!(waiting_label(VmOp::Restart), "restarting");
        assert_eq!(waiting_label(VmOp::RestartForce), "resetting");
        assert_eq!(waiting_label(VmOp::Save), "saving");
        assert_eq!(waiting_label(VmOp::Pause), "pausing");
        assert_eq!(waiting_label(VmOp::Resume), "resuming");
    }
}
