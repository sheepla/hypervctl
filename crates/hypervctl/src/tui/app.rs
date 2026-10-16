//! The main loop (design 5.2 and 5.10): every key, timer and result becomes an
//! action; the state is reduced and one frame is drawn per batch.

use std::time::Duration;

use crossterm::event::{self, Event};
use hypervctl_core::{
    BackendHandle, ChangeEvent, ChangeWatcher, HyperVError, Operations, RawError,
};
use ratatui_image::picker::{Picker, ProtocolType};
use tokio::sync::mpsc;
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::cli::commands::CommandOutput;
use crate::cli::errors::CliError;
use crate::tui::effects;
use crate::tui::store::{self, Action, Effect, State};
use crate::tui::{terminal, ui};

/// How long the pause between two UI ticks is; they only run while something
/// has a deadline (design 5.2).
const UI_TICK: Duration = Duration::from_millis(100);

/// Everything the loop needs that outlives one action.
struct Loop {
    ops: Operations,
    handle: BackendHandle,
    /// The dedicated actor the Thumbnail fetches run on, so they never queue
    /// behind a command (spec 3.10, design 5.13).
    thumbnails: BackendHandle,
    /// The terminal's image protocol, or `None` when it can only do half
    /// blocks (spec 3.10).
    picker: Option<Picker>,
    actions: mpsc::UnboundedSender<Action>,
}

/// Runs the TUI until the user quits or the backend stops; the terminal is
/// restored on every way out (design 5.10).
pub async fn run(
    ops: &Operations,
    refresh: Duration,
    mock: bool,
) -> Result<CommandOutput, CliError> {
    // Step 3: the first list is read before the terminal is touched, so a
    // failing backend is just a CLI error (design 5.10).
    let items = ops.handle().list_vms().await?;

    // Step 4: raw mode and the alternate screen go on only now.
    let restore = Box::leak(Box::new(terminal::Restore::for_terminal()));
    let mut terminal = terminal::enter().map_err(|error| io_error("entering the TUI", error))?;
    let guard = terminal::Guard::new(restore);
    terminal::set_panic_hook(restore);
    let size = terminal
        .size()
        .map_err(|error| io_error("reading the terminal size", error))?;
    let mut state = State::new(items, size.width, size.height);
    // The list title says `[mock]` when the mock backend runs (spec 3.2), and
    // the Monitor window follows the refresh interval (design 5.12).
    state.ui.mock = mock;
    state.ui.refresh = refresh;
    // The terminal's image protocol is detected once, after the alternate
    // screen is up but before the key reader starts, so the query sees its own
    // answers (spec 3.10, design 5.13).
    let picker = detect_picker();
    state.ui.image_supported = picker.is_some();
    let thumbnails = crate::spawn_thumbnail_backend();

    // Step 5: change notifications, keys and the timers.
    let (actions, mut incoming) = mpsc::unbounded_channel();
    let (changes, mut change_rx) = mpsc::unbounded_channel();
    let _watcher = ChangeWatcher::spawn(ops.handle().clone(), changes);
    let (key_tx, mut key_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || reader(key_tx));
    let mut refresh_timer = time::interval_at(Instant::now() + refresh, refresh);
    refresh_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut ui_timer = time::interval(UI_TICK);
    ui_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let looping = Loop {
        ops: ops.clone(),
        handle: ops.handle().clone(),
        thumbnails,
        picker,
        actions,
    };

    // The first list already selected a VM, but nothing has asked for its
    // detail yet (design 5.10).
    if let Some(id) = state.vm_list.selected {
        effects::spawn(
            Effect::FetchVmDetail(id),
            &looping.ops,
            &looping.handle,
            &looping.thumbnails,
            looping.picker.as_ref(),
            &looping.actions,
        );
    }

    // Step 6: wait for one event, apply it and everything else that piled up,
    // then draw once (design 5.2).
    loop {
        terminal
            .draw(|frame| ui::draw(frame, &state))
            .map_err(|error| io_error("drawing", error))?;
        tokio::select! {
            maybe = incoming.recv() => {
                if let Some(action) = maybe {
                    if !step(&mut state, action, &looping) {
                        break;
                    }
                }
            }
            maybe = key_rx.recv() => {
                match maybe {
                    Some(Event::Key(key)) => {
                        if !step(&mut state, Action::Input(key), &looping) {
                            break;
                        }
                    }
                    Some(Event::Resize(width, height)) => {
                        if !step(&mut state, Action::Resize(width, height), &looping) {
                            break;
                        }
                    }
                    // Mouse and paste events carry no action of their own.
                    Some(_) | None => {}
                }
            }
            maybe = change_rx.recv() => {
                match maybe {
                    Some(ChangeEvent::Vms) => {
                        if !step(&mut state, Action::BackendChanged, &looping) {
                            break;
                        }
                    }
                    Some(ChangeEvent::Checkpoints(vm))
                        if !step(&mut state, Action::CheckpointsChanged(vm), &looping) =>
                    {
                        break;
                    }
                    Some(ChangeEvent::Checkpoints(_)) | None => {}
                }
            }
            _ = refresh_timer.tick() => {
                if !step(&mut state, Action::RefreshTick, &looping) {
                    break;
                }
            }
            // The tick only runs while a message has a deadline (design 5.2).
            _ = ui_timer.tick(), if state.needs_ui_tick() => {
                if !step(&mut state, Action::UiTick(Instant::now().into_std()), &looping) {
                    break;
                }
            }
        }
        // Everything the batch produced is applied before the next frame.
        while let Ok(action) = incoming.try_recv() {
            if !step(&mut state, action, &looping) {
                break;
            }
        }
        if let Some(fatal) = state.fatal.take() {
            // The guard restores the terminal when this returns.
            drop(guard);
            return Err(CliError::Backend(fatal));
        }
    }
    drop(guard);
    Ok(CommandOutput {
        stdout: String::new(),
        stderr: String::new(),
        code: 0,
    })
}

/// One action through the reducer, with the effects it asks for; reports
/// whether the loop goes on.
fn step(state: &mut State, action: Action, looping: &Loop) -> bool {
    for effect in store::reduce(state, action) {
        match effect {
            Effect::Quit => return false,
            other => effects::spawn(
                other,
                &looping.ops,
                &looping.handle,
                &looping.thumbnails,
                looping.picker.as_ref(),
                &looping.actions,
            ),
        }
    }
    true
}

/// Reads the terminal on a thread of its own: `event::read` waits for a key,
/// which would stop the async loop from ever polling anything else.
fn reader(keys: mpsc::UnboundedSender<Event>) {
    while let Ok(event) = event::read() {
        if keys.send(event).is_err() {
            break;
        }
    }
}

fn io_error(what: &str, error: std::io::Error) -> CliError {
    CliError::Backend(HyperVError::backend(RawError::new(format!(
        "{what}: {error}"
    ))))
}

/// The terminal's image protocol, or `None` when it can only do half blocks
/// (spec 3.10). Detection never fails the run: a terminal that does not answer
/// simply looks unsupported and the Thumbnail tab keeps its placeholder.
fn detect_picker() -> Option<Picker> {
    match Picker::from_query_stdio() {
        Ok(picker) if picker.protocol_type() != ProtocolType::Halfblocks => Some(picker),
        _ => None,
    }
}
