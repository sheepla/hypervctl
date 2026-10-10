//! The terminal is taken over exactly once and brought back exactly once, on
//! every way out: the normal return, an error, and a panic (design 5.10,
//! acceptance 15).

use std::io::Stdout;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// What brings the terminal back to normal, callable at most once (design 5.10).
pub struct Restore {
    done: AtomicBool,
    restore: Box<dyn Fn() + Send + Sync>,
}

impl Restore {
    /// Wraps a restoration. The closure is what the tests drive; the real
    /// terminal only appears in [`Restore::for_terminal`].
    pub fn new(restore: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            done: AtomicBool::new(false),
            restore: Box::new(restore),
        }
    }

    /// The restoration of the real terminal: leave the alternate screen and the
    /// mouse capture, then raw mode.
    pub fn for_terminal() -> Self {
        Self::new(|| {
            let mut out = std::io::stdout();
            let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
            let _ = disable_raw_mode();
        })
    }

    /// Brings the terminal back. The first caller does the work; the rest do
    /// nothing, so the guard, the panic hook and the loop can all call it.
    pub fn restore(&self) {
        if !self.done.swap(true, Ordering::SeqCst) {
            (self.restore)();
        }
    }
}

/// The terminal, while the TUI runs: dropping it brings the terminal back, so
/// every `?` in the loop restores the terminal on the way out (design 5.10).
pub struct Guard<'a> {
    restore: &'a Restore,
}

impl<'a> Guard<'a> {
    pub fn new(restore: &'a Restore) -> Self {
        Self { restore }
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.restore.restore();
    }
}

/// Takes over the terminal: raw mode, alternate screen, mouse capture, then
/// the drawing surface (design 5.10 step 4).
pub fn enter() -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
    let mut out = std::io::stdout();
    enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(out))
}

/// Swaps in a hook that restores the terminal and then reports the panic the
/// usual way, so the message is not written into the alternate screen
/// (design 5.10, acceptance 15).
pub fn set_panic_hook(restore: &'static Restore) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        hook_body(restore, &default, info);
    }));
}

/// The body of the panic hook: the terminal first, the report second. The
/// payload is generic so that the tests can drive the order with a unit.
fn hook_body<P>(restore: &Restore, default: &(dyn Fn(&P) + Send + Sync + 'static), payload: &P) {
    restore.restore();
    default(payload);
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn the_restoration_runs_once_however_often_it_is_asked_for() {
        let calls = Arc::new(Mutex::new(0));
        let restore = Restore::new(counter(Arc::clone(&calls)));
        restore.restore();
        restore.restore();
        restore.restore();
        assert_eq!(count(&calls), 1);
    }

    #[test]
    fn the_guard_restores_on_the_way_out() {
        let calls = Arc::new(Mutex::new(0));
        let restore = Restore::new(counter(Arc::clone(&calls)));
        {
            let _guard = Guard::new(&restore);
        }
        assert_eq!(count(&calls), 1);
    }

    #[test]
    fn a_panic_hook_restores_the_terminal_before_reporting() {
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let restore = Restore::new({
            let order = Arc::clone(&order);
            move || {
                let mut order = order
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                order.push("restore");
            }
        });
        let default = {
            let order = Arc::clone(&order);
            move |_unit: &()| {
                let mut order = order
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                order.push("default");
            }
        };
        hook_body(&restore, &default, &());
        assert_eq!(
            *order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            vec!["restore", "default"]
        );
    }

    fn counter(calls: Arc<Mutex<u32>>) -> impl Fn() + Send + Sync + 'static {
        move || {
            let mut calls = calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *calls += 1;
        }
    }

    fn count(calls: &Arc<Mutex<u32>>) -> u32 {
        *calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
