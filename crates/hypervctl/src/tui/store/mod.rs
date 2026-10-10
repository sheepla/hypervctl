//! The state of the whole TUI and the pure reducer that moves it (design 5.2
//! and 5.3). State belongs to the main loop alone; `reduce` does no I/O, takes
//! no clock and returns the effects the runner has to perform.

use std::time::Instant;

use crossterm::event::KeyEvent;
use hypervctl_core::{HyperVError, VmDetail, VmId, VmSummary};

pub mod detail;
pub mod status;
pub mod vm_list;

use detail::DetailState;
pub use detail::DetailTab;
use status::{StatusKind, StatusState};
use vm_list::VmListState;

use crate::tui::keymap;

/// Which pane has the keyboard (spec 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Detail,
}

/// The terminal, as far as the state is concerned (design 5.3).
#[derive(Debug, Clone, Copy)]
pub struct UiState {
    pub width: u16,
    pub height: u16,
}

impl UiState {
    /// The smallest terminal the TUI draws anything but a notice on (spec 3.1).
    pub const MIN_WIDTH: u16 = 80;
    pub const MIN_HEIGHT: u16 = 24;

    pub fn too_small(&self) -> bool {
        self.width < Self::MIN_WIDTH || self.height < Self::MIN_HEIGHT
    }
}

/// Everything the TUI knows (design 5.3).
#[derive(Debug)]
pub struct State {
    pub vm_list: VmListState,
    pub focus: Focus,
    pub detail: DetailState,
    pub status: StatusState,
    pub ui: UiState,
    /// The backend actor stopped: the loop ends and the exit code is 1
    /// (design 5.5).
    pub fatal: Option<HyperVError>,
}

impl State {
    /// The state the first frame is drawn from: the list the caller fetched
    /// before the terminal was touched (design 5.10).
    pub fn new(items: Vec<VmSummary>, width: u16, height: u16) -> Self {
        let mut vm_list = VmListState {
            items,
            ..VmListState::default()
        };
        vm_list
            .items
            .sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        vm_list.selected = vm_list.items.first().map(|vm| vm.id);
        Self {
            vm_list,
            focus: Focus::List,
            detail: DetailState {
                tab: DetailTab::Details,
                for_vm: None,
                detail: None,
                scroll: 0,
            },
            status: StatusState::default(),
            ui: UiState { width, height },
            fatal: None,
        }
    }

    /// Which keymap table the keys are looked up in right now (spec 3.3).
    pub fn keymap_context(&self) -> keymap::Context {
        match self.focus {
            Focus::List => keymap::Context::List,
            Focus::Detail => keymap::Context::Detail,
        }
    }

    /// Whether the `UiTick` has to run: only while something expires (design 5.2).
    pub fn needs_ui_tick(&self) -> bool {
        self.status.needs_tick()
    }
}

/// What the runner has to do for the reducer (design 5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    FetchVmList,
    FetchVmDetail(VmId),
    Quit,
}

/// Everything that moves the state (design 5.2).
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    // From the keymap (spec 3.3).
    Quit,
    FocusToggle,
    FocusList,
    FocusDetail,
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    SelectTab(DetailTab),
    TabNext,
    TabPrev,
    ScrollUp,
    ScrollDown,
    RefreshNow,
    // From the terminal and the timers.
    Input(KeyEvent),
    Resize(u16, u16),
    RefreshTick,
    UiTick(Instant),
    BackendChanged,
    // The results of the effects. The payloads are boxed so that the action
    // itself stays small enough to shuffle around the loop.
    VmListLoaded(Box<Result<Vec<VmSummary>, HyperVError>>),
    VmDetailLoaded {
        id: VmId,
        result: Box<Result<VmDetail, HyperVError>>,
    },
}

/// One step of the state machine (design 5.2).
pub fn reduce(state: &mut State, action: Action) -> Vec<Effect> {
    let mut effects = Vec::new();
    match action {
        Action::Quit => effects.push(Effect::Quit),
        Action::FocusToggle => {
            state.focus = match state.focus {
                Focus::List => Focus::Detail,
                Focus::Detail => Focus::List,
            };
        }
        Action::FocusList => state.focus = Focus::List,
        Action::FocusDetail => state.focus = Focus::Detail,
        Action::SelectNext => {
            state.vm_list.move_by(1);
            follow_selection(state, &mut effects);
        }
        Action::SelectPrev => {
            state.vm_list.move_by(-1);
            follow_selection(state, &mut effects);
        }
        Action::SelectFirst => {
            state.vm_list.select_first();
            follow_selection(state, &mut effects);
        }
        Action::SelectLast => {
            state.vm_list.select_last();
            follow_selection(state, &mut effects);
        }
        Action::SelectTab(tab) => state.detail.tab = tab,
        Action::TabNext => state.detail.tab = state.detail.tab.next(),
        Action::TabPrev => state.detail.tab = state.detail.tab.previous(),
        Action::ScrollDown => state.detail.scroll = state.detail.scroll.saturating_add(1),
        Action::ScrollUp => state.detail.scroll = state.detail.scroll.saturating_sub(1),
        Action::RefreshNow => request_list_now(state, &mut effects),
        // A raw key reaches the keymap here, so the mapping stays a pure table
        // lookup and only presses ever become actions (design 5.8, acceptance 12).
        Action::Input(key) => {
            if let Some(action) = keymap::action(state.keymap_context(), key) {
                effects.extend(reduce(state, action));
            }
        }
        Action::Resize(width, height) => {
            state.ui.width = width;
            state.ui.height = height;
        }
        Action::RefreshTick => {
            // While a fetch is running, a tick starts nothing (design 5.5,
            // acceptance 11).
            if !state.vm_list.refresh_in_flight {
                start_list_fetch(state, &mut effects);
            }
        }
        Action::UiTick(now) => state.status.expire(now),
        Action::BackendChanged => request_list_now(state, &mut effects),
        Action::VmListLoaded(result) => {
            state.vm_list.refresh_in_flight = false;
            match *result {
                Ok(items) => {
                    if let Some(name) = state.vm_list.apply(items) {
                        state.status.show(
                            StatusKind::Info,
                            format!("VM '{name}' is no longer available"),
                        );
                    }
                    // The selection may have changed, and every list update
                    // re-reads the selected VM's detail (design 5.5).
                    state.detail.follow(state.vm_list.selected);
                    if let Some(id) = state.vm_list.selected {
                        effects.push(Effect::FetchVmDetail(id));
                    }
                }
                Err(error) if error.is_backend_stopped() => state.fatal = Some(error),
                // Keep the last list on screen and warn (design 5.5).
                Err(error) => state.vm_list.stale = Some(error.to_string()),
            }
            if state.vm_list.refresh_again {
                state.vm_list.refresh_again = false;
                start_list_fetch(state, &mut effects);
            }
        }
        Action::VmDetailLoaded { id, result } => {
            // A late answer for a VM that is no longer selected is dropped
            // (design 5.5, acceptance 10).
            if state.vm_list.selected != Some(id) {
                return effects;
            }
            match *result {
                Ok(detail) => state.detail.show(id, detail),
                Err(error) if error.is_backend_stopped() => state.fatal = Some(error),
                Err(error) => state.status.show(StatusKind::Error, error.to_string()),
            }
        }
    }
    effects
}

/// The detail pane follows a selection that just moved (design 5.5).
fn follow_selection(state: &mut State, effects: &mut Vec<Effect>) {
    let before = state.detail.for_vm;
    state.detail.follow(state.vm_list.selected);
    if state.detail.for_vm != before {
        if let Some(id) = state.detail.for_vm {
            effects.push(Effect::FetchVmDetail(id));
        }
    }
}

/// An immediate refresh: a second fetch while one runs is remembered instead
/// of started (design 5.5).
fn request_list_now(state: &mut State, effects: &mut Vec<Effect>) {
    if state.vm_list.refresh_in_flight {
        state.vm_list.refresh_again = true;
    } else {
        start_list_fetch(state, effects);
    }
}

fn start_list_fetch(state: &mut State, effects: &mut Vec<Effect>) {
    state.vm_list.refresh_in_flight = true;
    effects.push(Effect::FetchVmList);
}

#[cfg(test)]
mod tests {
    use hypervctl_core::{stopped_error, MemoryConfig, RawError, VmDetail, VmState};
    use uuid::Uuid;

    use super::*;

    fn id(index: u128) -> VmId {
        VmId::new(Uuid::from_u128(index << 96))
    }

    fn vm(index: u128, name: &str) -> VmSummary {
        VmSummary {
            id: id(index),
            name: name.to_string(),
            state: VmState::Off,
            cpu_usage_percent: None,
            memory_assigned_bytes: None,
            uptime: None,
            generation: None,
        }
    }

    fn detail(summary: VmSummary) -> VmDetail {
        VmDetail {
            summary,
            processor_count: 1,
            memory: MemoryConfig {
                startup_bytes: 1,
                dynamic_enabled: false,
                min_bytes: None,
                max_bytes: None,
            },
            disks: Vec::new(),
            nics: Vec::new(),
            checkpoint_count: 0,
            config_path: String::new(),
            integration_services: Vec::new(),
            notes: None,
        }
    }

    fn state_with(items: Vec<VmSummary>) -> State {
        State::new(items, 80, 24)
    }

    fn list_loaded(state: &mut State, items: Vec<VmSummary>) -> Vec<Effect> {
        reduce(state, Action::VmListLoaded(Box::new(Ok(items))))
    }

    fn detail_loaded(state: &mut State, id: VmId, detail: VmDetail) -> Vec<Effect> {
        reduce(
            state,
            Action::VmDetailLoaded {
                id,
                result: Box::new(Ok(detail)),
            },
        )
    }

    fn detail_failed(state: &mut State, id: VmId, error: HyperVError) -> Vec<Effect> {
        reduce(
            state,
            Action::VmDetailLoaded {
                id,
                result: Box::new(Err(error)),
            },
        )
    }

    #[test]
    fn the_selection_is_kept_by_id_when_the_list_changes() {
        let mut state = state_with(vec![vm(1, "web-01"), vm(2, "web-02")]);
        reduce(&mut state, Action::SelectNext);
        assert_eq!(state.vm_list.selected, Some(id(2)), "web-02 is selected");

        // The same VMs, in a different order: the selection must not follow
        // the row (acceptance 9).
        list_loaded(&mut state, vec![vm(2, "web-02"), vm(1, "web-01")]);
        assert_eq!(
            state.vm_list.selected,
            Some(id(2)),
            "the order does not matter"
        );
        assert_eq!(state.status.current, None, "nothing vanished");
    }

    #[test]
    fn a_vanished_selection_falls_back_to_the_nearest_row() {
        // Sorted: db-01, web-01, web-02.
        let mut state = state_with(vec![vm(1, "web-01"), vm(2, "web-02"), vm(3, "db-01")]);
        reduce(&mut state, Action::SelectFirst);
        assert_eq!(state.vm_list.selected, Some(id(3)), "db-01 sorts first");

        list_loaded(&mut state, vec![vm(1, "web-01"), vm(2, "web-02")]);
        assert_eq!(
            state.vm_list.selected,
            Some(id(1)),
            "the nearest row is web-01"
        );
        let message = state
            .status
            .current
            .as_ref()
            .unwrap_or_else(|| panic!("the user is told about the vanished VM"));
        assert_eq!(message.text, "VM 'db-01' is no longer available");
    }

    #[test]
    fn an_empty_list_clears_the_selection() {
        let mut state = state_with(vec![vm(1, "web-01")]);
        list_loaded(&mut state, Vec::new());
        assert_eq!(state.vm_list.selected, None);
        assert!(state.detail.detail.is_none());
    }

    #[test]
    fn a_late_detail_of_another_vm_is_discarded() {
        let mut state = state_with(vec![vm(1, "web-01"), vm(2, "web-02")]);
        let first = state
            .vm_list
            .selected
            .unwrap_or_else(|| panic!("the first VM is selected right after the state is built"));
        detail_loaded(&mut state, first, detail(vm(1, "web-01")));
        assert!(state.detail.detail.is_some());

        reduce(&mut state, Action::SelectNext);
        let second = state.vm_list.selected.unwrap_or_else(|| panic!("web-02"));
        assert!(
            state.detail.detail.is_none(),
            "the pane is cleared on the move"
        );

        // The answer for the first VM arrives late and belongs to no selection
        // any more (acceptance 10).
        let effects = detail_loaded(&mut state, first, detail(vm(1, "web-01")));
        assert!(effects.is_empty());
        assert!(state.detail.detail.is_none(), "the stale detail is dropped");

        // The answer for the selected VM is kept.
        detail_loaded(&mut state, second, detail(vm(2, "web-02")));
        assert!(state.detail.detail.is_some());
    }

    #[test]
    fn a_tick_starts_no_second_fetch_and_a_manual_refresh_waits_its_turn() {
        let mut state = state_with(vec![vm(1, "web-01")]);
        let effects = reduce(&mut state, Action::RefreshTick);
        assert_eq!(effects, vec![Effect::FetchVmList]);
        assert!(state.vm_list.refresh_in_flight);

        // A tick while the fetch runs does nothing (acceptance 11).
        assert_eq!(reduce(&mut state, Action::RefreshTick), Vec::new());

        // A manual refresh while the fetch runs is remembered once.
        assert_eq!(reduce(&mut state, Action::RefreshNow), Vec::new());
        assert!(state.vm_list.refresh_again);

        // The answer completes the fetch; the remembered refresh runs now.
        let effects = list_loaded(&mut state, vec![vm(1, "web-01")]);
        assert!(!state.vm_list.refresh_again);
        assert!(
            state.vm_list.refresh_in_flight,
            "the remembered refresh started"
        );
        assert_eq!(
            effects,
            vec![Effect::FetchVmDetail(id(1)), Effect::FetchVmList],
            "the detail of the selection and the remembered refresh"
        );
    }

    #[test]
    fn a_backend_change_refreshes_immediately() {
        let mut state = state_with(vec![vm(1, "web-01")]);
        let effects = reduce(&mut state, Action::BackendChanged);
        assert_eq!(effects, vec![Effect::FetchVmList]);
        assert!(state.vm_list.refresh_in_flight);
    }

    #[test]
    fn a_backend_that_stopped_ends_the_tui_with_an_error() {
        let mut state = state_with(vec![vm(1, "web-01")]);
        reduce(
            &mut state,
            Action::VmListLoaded(Box::new(Err(stopped_error()))),
        );
        assert!(
            state
                .fatal
                .as_ref()
                .is_some_and(HyperVError::is_backend_stopped),
            "a stopped actor is fatal (design 5.5)"
        );

        let mut state = state_with(vec![vm(1, "web-01")]);
        detail_failed(&mut state, id(1), stopped_error());
        assert!(state.fatal.is_some());
    }

    #[test]
    fn any_other_failure_keeps_the_screen_and_says_the_list_is_out_of_date() {
        let mut state = state_with(vec![vm(1, "web-01")]);
        reduce(
            &mut state,
            Action::VmListLoaded(Box::new(Err(HyperVError::backend(RawError::new(
                "wmi is angry",
            ))))),
        );
        assert!(state.fatal.is_none());
        assert!(state
            .vm_list
            .stale
            .as_ref()
            .is_some_and(|stale| stale.contains("wmi is angry")));

        let mut state = state_with(vec![vm(1, "web-01")]);
        detail_failed(
            &mut state,
            id(1),
            HyperVError::backend(RawError::new("wmi is angry")),
        );
        assert!(state.fatal.is_none());
        let message = state
            .status
            .current
            .as_ref()
            .unwrap_or_else(|| panic!("the failure is shown"));
        assert_eq!(message.kind, StatusKind::Error);
    }

    #[test]
    fn the_keys_of_the_list_reach_the_keymap_and_the_detail_follows() {
        let mut state = state_with(vec![vm(1, "web-01"), vm(2, "web-02")]);
        let effects = reduce(
            &mut state,
            Action::Input(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('j'),
                crossterm::event::KeyModifiers::empty(),
            )),
        );
        assert_eq!(state.vm_list.selected, Some(id(2)), "'j' moves down");
        assert_eq!(
            effects,
            vec![Effect::FetchVmDetail(id(2))],
            "the detail follows"
        );

        // Enter focuses the detail pane; Tab toggles back.
        reduce(
            &mut state,
            Action::Input(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            )),
        );
        assert_eq!(state.focus, Focus::Detail);
        reduce(
            &mut state,
            Action::Input(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::empty(),
            )),
        );
        assert_eq!(state.focus, Focus::List);
    }

    #[test]
    fn scrolling_the_detail_never_goes_below_zero() {
        let mut state = state_with(vec![vm(1, "web-01")]);
        reduce(&mut state, Action::ScrollUp);
        assert_eq!(state.detail.scroll, 0);
        reduce(&mut state, Action::ScrollDown);
        reduce(&mut state, Action::ScrollDown);
        assert_eq!(state.detail.scroll, 2);
        reduce(&mut state, Action::ScrollUp);
        assert_eq!(state.detail.scroll, 1);
    }
}
