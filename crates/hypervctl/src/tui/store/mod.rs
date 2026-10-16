//! The state of the whole TUI and the pure reducer that moves it (design 5.2
//! and 5.3). State belongs to the main loop alone; `reduce` does no I/O, takes
//! no clock and returns the effects the runner has to perform.

use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use hypervctl_core::{Checkpoint, HyperVError, VmDetail, VmId, VmState, VmSummary};

pub mod detail;
pub mod menu;
pub mod ops_state;
pub mod overlay;
pub mod status;
pub mod vm_list;

use detail::DetailState;
pub use detail::DetailTab;
use menu::{
    checkpoint_confirm_question, checkpoint_item_enabled, vm_confirm_question, vm_item_enabled,
    CheckpointMenuItem, VmMenuItem, CHECKPOINT_MENU, VM_MENU,
};
use ops_state::OpsState;
pub use overlay::{ConfirmState, MenuKind, MenuState, OpRequest, Overlay};
use status::{StatusKind, StatusState};
use vm_list::VmListState;

use crate::tui::features;
use crate::tui::keymap;
use crate::tui::ui::{layout, scroll};

/// How long a success stays on the status bar before it fades (spec 3.5).
const SUCCESS_LIFETIME: Duration = Duration::from_secs(3);

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
    /// Which spinner frame the operations in flight show (spec 3.5).
    pub spinner: usize,
    /// The pane rectangles of spec 3.2, recomputed on every resize and every
    /// list change; the view draws exactly what the scroll arithmetic used
    /// (design 5.11).
    pub layout: layout::Layout,
    /// Whether the mock backend is running: the list title says `[mock]`
    /// (spec 3.2).
    pub mock: bool,
    /// The clock the `UiTick` advances: ages like `stale 12s` are read from
    /// here (spec 3.2).
    pub now: Instant,
}

impl UiState {
    /// The smallest terminal the TUI draws anything but a notice on (spec 3.1).
    pub const MIN_WIDTH: u16 = 80;
    pub const MIN_HEIGHT: u16 = 24;

    pub fn too_small(&self) -> bool {
        self.width < Self::MIN_WIDTH || self.height < Self::MIN_HEIGHT
    }
}

/// A failed operation, with the classification kept so that a stopped backend
/// stays fatal (design 5.5).
#[derive(Debug, Clone, PartialEq)]
pub struct OpFailure {
    pub message: String,
    pub error: HyperVError,
}

/// Everything the TUI knows (design 5.3).
#[derive(Debug)]
pub struct State {
    pub vm_list: VmListState,
    pub focus: Focus,
    pub detail: DetailState,
    pub ops: OpsState,
    pub overlay: Overlay,
    pub status: StatusState,
    pub ui: UiState,
    /// How far the help overlay is scrolled (design 5.11).
    pub help_scroll: u16,
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
        vm_list.recompute_visible();
        let vm_count = vm_list.visible.len();
        Self {
            vm_list,
            focus: Focus::List,
            detail: DetailState {
                tab: DetailTab::Details,
                for_vm: None,
                detail: None,
                checkpoints: detail::CheckpointsState::default(),
                scroll: 0,
            },
            ops: OpsState::default(),
            overlay: Overlay::None,
            status: StatusState::default(),
            ui: UiState {
                width,
                height,
                spinner: 0,
                layout: layout::compute(width, height, vm_count),
                mock: false,
                now: Instant::now(),
            },
            help_scroll: 0,
            fatal: None,
        }
    }

    /// Which keymap table the keys are looked up in right now (spec 3.3). A
    /// popup outranks the panes; the filter input takes the keyboard whole
    /// while it is up (spec 3.3).
    pub fn keymap_context(&self) -> keymap::Context {
        match self.overlay {
            Overlay::Menu(_) => keymap::Context::Menu,
            Overlay::Confirm(_) => keymap::Context::Confirm,
            Overlay::Help => keymap::Context::Help,
            Overlay::None => {
                if self.vm_list.filter.editing {
                    return keymap::Context::Filter;
                }
                match self.focus {
                    Focus::List => keymap::Context::List,
                    Focus::Detail => keymap::Context::Detail,
                }
            }
        }
    }

    /// Whether the `UiTick` has to run: while a message expires, an
    /// operation spins or the `stale 12s` age ticks (design 5.2, spec 3.2
    /// and 3.5).
    pub fn needs_ui_tick(&self) -> bool {
        self.status.needs_tick() || !self.ops.is_empty() || self.vm_list.stale_since.is_some()
    }
}

/// What the runner has to do for the reducer (design 5.4).
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    FetchVmList,
    FetchVmDetail(VmId),
    FetchCheckpoints(VmId),
    RunOperation(OpRequest),
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
    ScrollTop,
    ScrollBottom,
    RefreshNow,
    OpenMenu,
    MenuUp,
    MenuDown,
    MenuSelect,
    ConfirmYes,
    ConfirmCancel,
    ToggleHelp,
    CloseOverlay,
    // The filter input line (spec 3.3, design 5.11).
    FilterStart,
    FilterChar(char),
    FilterBackspace,
    FilterClear,
    FilterApply,
    FilterCancel,
    // From the terminal, the timers and the change notifications.
    Input(KeyEvent),
    Resize(u16, u16),
    RefreshTick,
    UiTick(Instant),
    BackendChanged,
    CheckpointsChanged(VmId),
    // The results of the effects. The payloads are boxed so that the action
    // itself stays small enough to shuffle around the loop.
    VmListLoaded(Box<Result<Vec<VmSummary>, HyperVError>>),
    VmDetailLoaded {
        id: VmId,
        result: Box<Result<VmDetail, HyperVError>>,
    },
    CheckpointsLoaded {
        vm: VmId,
        result: Box<Result<Vec<Checkpoint>, HyperVError>>,
    },
    OpFinished {
        vm: VmId,
        result: Result<String, OpFailure>,
        at: Instant,
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
        Action::SelectTab(tab) => {
            state.detail.tab = tab;
            if tab == DetailTab::Checkpoints {
                request_checkpoints(state, &mut effects);
            }
        }
        Action::TabNext => {
            state.detail.tab = state.detail.tab.next();
            if state.detail.tab == DetailTab::Checkpoints {
                request_checkpoints(state, &mut effects);
            }
        }
        Action::TabPrev => {
            state.detail.tab = state.detail.tab.previous();
            if state.detail.tab == DetailTab::Checkpoints {
                request_checkpoints(state, &mut effects);
            }
        }
        Action::ScrollDown => scroll_by(state, 1),
        Action::ScrollUp => scroll_by(state, -1),
        // `g`/`G`: first and last (spec 3.3) — of the checkpoints when that
        // list is up, of the help when it covers the panes, of the detail
        // body else. Overshooting is clamped at the end of the reduction.
        Action::ScrollTop => scroll_to_edge(state, false),
        Action::ScrollBottom => scroll_to_edge(state, true),
        Action::RefreshNow => request_list_now(state, &mut effects),
        Action::OpenMenu => open_menu(state),
        Action::MenuUp => menu_move(state, -1),
        Action::MenuDown => menu_move(state, 1),
        Action::MenuSelect => menu_select(state, &mut effects),
        Action::ConfirmYes => confirm_yes(state, &mut effects),
        Action::ConfirmCancel => {
            if matches!(state.overlay, Overlay::Confirm(_)) {
                state.overlay = Overlay::None;
            }
        }
        Action::ToggleHelp => {
            state.overlay = match &state.overlay {
                Overlay::Help => Overlay::None,
                Overlay::None => {
                    state.help_scroll = 0;
                    Overlay::Help
                }
                // A menu or a dialog in front keeps its place (spec 3.3).
                other => other.clone(),
            };
        }
        Action::CloseOverlay => {
            if !matches!(state.overlay, Overlay::None) {
                state.overlay = Overlay::None;
            }
        }
        Action::FilterStart => state.vm_list.filter.editing = true,
        Action::FilterChar(character) => {
            state.vm_list.filter.text.push(character);
            filter_changed(state, &mut effects);
        }
        Action::FilterBackspace => {
            state.vm_list.filter.text.pop();
            filter_changed(state, &mut effects);
        }
        Action::FilterClear => {
            state.vm_list.filter.text.clear();
            filter_changed(state, &mut effects);
        }
        Action::FilterApply => state.vm_list.filter.editing = false,
        Action::FilterCancel => {
            state.vm_list.filter.editing = false;
            state.vm_list.filter.text.clear();
            filter_changed(state, &mut effects);
        }
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
            recompute_layout(state);
        }
        Action::RefreshTick => {
            // While a fetch is running, a tick starts nothing (design 5.5,
            // acceptance 11).
            if !state.vm_list.refresh_in_flight {
                start_list_fetch(state, &mut effects);
            }
        }
        Action::UiTick(now) => {
            state.ui.now = now;
            state.status.expire(now);
            state.ui.spinner = (state.ui.spinner + 1) % 4;
        }
        Action::BackendChanged => request_list_now(state, &mut effects),
        Action::CheckpointsChanged(vm) => {
            // The checkpoints of the VM on screen were announced as changed
            // (spec 3.7); everything else waits for the tab.
            if state.detail.tab == DetailTab::Checkpoints && state.vm_list.selected == Some(vm) {
                request_checkpoints(state, &mut effects);
            }
        }
        Action::VmListLoaded(result) => {
            state.vm_list.refresh_in_flight = false;
            match *result {
                Ok(items) => {
                    // The name has to be read before the list is replaced, or a
                    // vanished popup target cannot be told apart from an unknown
                    // one (design 5.6).
                    let overlay_target = state.overlay.target();
                    let overlay_name =
                        overlay_target.and_then(|target| state.vm_list.name_of(target));
                    let selection_vanished = state.vm_list.apply(items);
                    // The count feeds the pane heights and the `rows a-b`
                    // label (spec 3.2).
                    recompute_layout(state);
                    if let Some(name) = &selection_vanished {
                        state.status.show(
                            StatusKind::Info,
                            format!("VM '{name}' is no longer available"),
                        );
                    }
                    close_overlay_of_a_vanished_vm(
                        state,
                        overlay_target,
                        overlay_name,
                        &selection_vanished,
                    );
                    // The selection may have changed, and every list update
                    // re-reads the selected VM's detail (design 5.5).
                    state.detail.follow(state.vm_list.selected);
                    if let Some(id) = state.vm_list.selected {
                        effects.push(Effect::FetchVmDetail(id));
                    }
                    if state.detail.tab == DetailTab::Checkpoints {
                        request_checkpoints(state, &mut effects);
                    }
                }
                Err(error) if error.is_backend_stopped() => state.fatal = Some(error),
                // Keep the last list on screen and warn (design 5.5). The
                // age in the title starts now (spec 3.2).
                Err(error) => {
                    let now = state.ui.now;
                    state.vm_list.stale = Some(error.to_string());
                    state.vm_list.stale_since = Some(now);
                }
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
        Action::CheckpointsLoaded { vm, result } => {
            state.detail.checkpoints.in_flight = false;
            // A late answer for a VM that is no longer on screen is dropped
            // the same way a detail is (design 5.5).
            if state.detail.checkpoints.for_vm != Some(vm) {
                return effects;
            }
            match *result {
                Ok(items) => {
                    if let Some(name) = state.detail.checkpoints.apply(vm, items) {
                        state.status.show(
                            StatusKind::Info,
                            format!("Checkpoint '{name}' is no longer available"),
                        );
                    }
                }
                Err(error) if error.is_backend_stopped() => state.fatal = Some(error),
                Err(error) => state.status.show(StatusKind::Error, error.to_string()),
            }
        }
        Action::OpFinished { vm, result, at } => {
            state.ops.finish(vm);
            match result {
                Ok(message) => {
                    state
                        .status
                        .show_timed(StatusKind::Info, message, at + SUCCESS_LIFETIME);
                }
                Err(failure) => {
                    if failure.error.is_backend_stopped() {
                        state.fatal = Some(failure.error.clone());
                    }
                    state.status.show(StatusKind::Error, failure.message);
                }
            }
            // Whatever the operation did, the list is behind now (spec 3.7).
            request_list_now(state, &mut effects);
        }
    }
    // Every action that can change a scroll target or a pane size ends with
    // the same arithmetic the view uses: the selection stays visible, the
    // detail and help scrolls stay inside their content (spec 3.3, design
    // 5.11).
    sync_scrolls(state);
    effects
}

/// The pane rectangles for the rows that are actually shown: the visible
/// count feeds the heights, so a filter change re-sizes the panes (spec 3.2,
/// design 5.11).
fn recompute_layout(state: &mut State) {
    state.ui.layout = layout::compute(state.ui.width, state.ui.height, state.vm_list.visible.len());
}

/// The filter text just changed: the visible rows follow, the selection jumps
/// to the first visible row when it fell out of the filter (spec 3.3), the
/// pane heights follow (design 5.11) and the detail tracks the new selection
/// (design 5.5). Fetching is never filtered (spec 3.3).
fn filter_changed(state: &mut State, effects: &mut Vec<Effect>) {
    state.vm_list.recompute_visible();
    state.vm_list.select_first_visible();
    recompute_layout(state);
    follow_selection(state, effects);
}

/// The scroll motion of `ScrollUp`/`ScrollDown`: the help when it is up, the
/// checkpoint list when that list is focused, the detail body else (spec 3.3).
fn scroll_by(state: &mut State, delta: isize) {
    if matches!(state.overlay, Overlay::Help) {
        if delta > 0 {
            state.help_scroll = state.help_scroll.saturating_add(1);
        } else {
            state.help_scroll = state.help_scroll.saturating_sub(1);
        }
        return;
    }
    if state.focus == Focus::Detail && state.detail.tab == DetailTab::Checkpoints {
        state.detail.checkpoints.move_by(delta);
        return;
    }
    if delta > 0 {
        state.detail.scroll = state.detail.scroll.saturating_add(1);
    } else {
        state.detail.scroll = state.detail.scroll.saturating_sub(1);
    }
}

/// The scroll motion of `g`/`G` (spec 3.3): jump to the far end of whichever
/// scroll target is active. Overshooting is clamped by `sync_scrolls`.
fn scroll_to_edge(state: &mut State, bottom: bool) {
    if matches!(state.overlay, Overlay::Help) {
        state.help_scroll = if bottom { u16::MAX } else { 0 };
        return;
    }
    if state.focus == Focus::Detail && state.detail.tab == DetailTab::Checkpoints {
        if bottom {
            state.detail.checkpoints.select_last();
        } else {
            state.detail.checkpoints.select_first();
        }
        return;
    }
    state.detail.scroll = if bottom { u16::MAX } else { 0 };
}

/// Clamp every scroll target against what is on screen now (spec 3.3): the
/// list keeps its selection visible, the detail and the help cannot scroll
/// past their content.
fn sync_scrolls(state: &mut State) {
    let rows = state.vm_list.visible.len();
    let height = layout::list_body_height(state.ui.layout);
    let index = state.vm_list.visible_index();
    state.vm_list.offset = scroll::visible_range(index, state.vm_list.offset, height, rows);
    // The checkpoints keep their own selection visible while that tab is up
    // (spec 3.8).
    if state.detail.tab == DetailTab::Checkpoints {
        let total = state.detail.checkpoints.items.len();
        let index = state.detail.checkpoints.selected_index();
        state.detail.checkpoints.offset = scroll::visible_range(
            index,
            state.detail.checkpoints.offset,
            layout::detail_body_height(state.ui.layout),
            total,
        );
    }
    let body_rows = features::detail::body_lines(state)
        .len()
        .min(u16::MAX as usize) as u16;
    state.detail.scroll = scroll::clamp(
        state.detail.scroll,
        body_rows,
        layout::detail_body_height(state.ui.layout),
    );
    let help_rows = features::help::content_lines(state.focus)
        .len()
        .min(u16::MAX as usize) as u16;
    state.help_scroll = scroll::clamp(
        state.help_scroll,
        help_rows,
        state.ui.height.saturating_sub(4),
    );
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

/// Reads the checkpoints of the VM on screen, once (design 5.5). The result is
/// filed under the VM it was asked for, so a late answer is recognisable.
fn request_checkpoints(state: &mut State, effects: &mut Vec<Effect>) {
    if let Some(id) = state.vm_list.selected {
        if !state.detail.checkpoints.in_flight {
            state.detail.checkpoints.in_flight = true;
            state.detail.checkpoints.for_vm = Some(id);
            effects.push(Effect::FetchCheckpoints(id));
        }
    }
}

/// Opens the action menu for whatever the popup rules say is the target
/// (spec 3.3): the checkpoint menu on the Checkpoints tab, which opens even
/// with nothing selected, the selected VM's menu else (spec 3.4).
fn open_menu(state: &mut State) {
    let Some(vm) = state.vm_list.selected else {
        return;
    };
    if state.focus == Focus::Detail && state.detail.tab == DetailTab::Checkpoints {
        let checkpoint = state
            .detail
            .checkpoints
            .selected_entry()
            .map(|entry| (entry.id, entry.name.clone()));
        state.overlay = Overlay::Menu(MenuState {
            vm,
            kind: MenuKind::Checkpoint,
            cursor: first_enabled_checkpoint_item(state, vm, checkpoint.is_some()),
            checkpoint,
        });
        return;
    }
    let cursor = first_enabled_vm_item(state, vm);
    state.overlay = Overlay::Menu(MenuState {
        vm,
        kind: MenuKind::Vm,
        checkpoint: None,
        cursor,
    });
}

/// The state of `vm` as the list on screen has it.
fn vm_state_of(state: &State, vm: VmId) -> Option<VmState> {
    state
        .vm_list
        .items
        .iter()
        .find(|entry| entry.id == vm)
        .map(|entry| entry.state)
}

/// The first menu row that can run, for a menu that just opened.
fn first_enabled_vm_item(state: &State, vm: VmId) -> usize {
    let Some(vm_state) = vm_state_of(state, vm) else {
        return 0;
    };
    let in_flight = state.ops.in_flight(vm);
    VM_MENU
        .iter()
        .position(|item| vm_item_enabled(*item, vm_state, in_flight))
        .unwrap_or(0)
}

/// The first checkpoint menu row that can run (spec 3.4), for the menu on the
/// Checkpoints tab. `Create checkpoint` needs no selection, so a menu with
/// nothing selected lands there (spec 3.8).
fn first_enabled_checkpoint_item(state: &State, vm: VmId, checkpoint_selected: bool) -> usize {
    let Some(vm_state) = vm_state_of(state, vm) else {
        return 0;
    };
    let in_flight = state.ops.in_flight(vm);
    CHECKPOINT_MENU
        .iter()
        .position(|item| {
            checkpoint_item_enabled(*item, Some(vm_state), checkpoint_selected, in_flight)
        })
        .unwrap_or(0)
}

/// The rows the cursor may stand on, given the current state (design 5.6:
/// computed every frame, so a menu that stays open follows the state).
fn enabled_menu_indices(state: &State, menu: &MenuState) -> Vec<usize> {
    match menu.kind {
        MenuKind::Checkpoint => {
            let Some(vm_state) = vm_state_of(state, menu.vm) else {
                return Vec::new();
            };
            let in_flight = state.ops.in_flight(menu.vm);
            let checkpoint_selected = menu.checkpoint.is_some();
            CHECKPOINT_MENU
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    checkpoint_item_enabled(**item, Some(vm_state), checkpoint_selected, in_flight)
                })
                .map(|(index, _)| index)
                .collect()
        }
        MenuKind::Vm => {
            let Some(vm_state) = vm_state_of(state, menu.vm) else {
                return Vec::new();
            };
            let in_flight = state.ops.in_flight(menu.vm);
            VM_MENU
                .iter()
                .enumerate()
                .filter(|(_, item)| vm_item_enabled(**item, vm_state, in_flight))
                .map(|(index, _)| index)
                .collect()
        }
    }
}

/// Moves the menu cursor over the rows that can run, never onto a grey one.
fn menu_move(state: &mut State, delta: isize) {
    let Overlay::Menu(menu) = &state.overlay else {
        return;
    };
    let (kind, cursor) = (menu.kind, menu.cursor);
    let draft = MenuState {
        vm: menu.vm,
        kind,
        checkpoint: menu.checkpoint.clone(),
        cursor,
    };
    let enabled = enabled_menu_indices(state, &draft);
    if enabled.is_empty() {
        return;
    }
    let position = enabled
        .iter()
        .position(|&index| index == cursor)
        .unwrap_or(0);
    let next = (position as isize + delta).clamp(0, enabled.len() as isize - 1) as usize;
    if let Overlay::Menu(menu) = &mut state.overlay {
        menu.cursor = enabled[next];
    }
}

/// Runs the highlighted menu row, or moves to the confirmation dialog first
/// (spec 3.4).
fn menu_select(state: &mut State, effects: &mut Vec<Effect>) {
    let Overlay::Menu(menu) = state.overlay.clone() else {
        return;
    };
    let Some(vm_name) = state.vm_list.name_of(menu.vm) else {
        // The target vanished between the draw and the key; the next list
        // update closes the menu (design 5.6).
        return;
    };
    match menu.kind {
        MenuKind::Vm => {
            let Some(item) = VM_MENU.get(menu.cursor).copied() else {
                return;
            };
            let Some(vm_state) = vm_state_of(state, menu.vm) else {
                return;
            };
            if !vm_item_enabled(item, vm_state, state.ops.in_flight(menu.vm)) {
                return;
            }
            if item.needs_confirm() {
                state.overlay = Overlay::Confirm(ConfirmState {
                    vm: menu.vm,
                    question: vm_confirm_question(item, &vm_name),
                    note: None,
                    request: OpRequest::Vm {
                        vm: menu.vm,
                        vm_name: vm_name.clone(),
                        op: item,
                    },
                });
                return;
            }
            let request = if item == VmMenuItem::CreateCheckpoint {
                OpRequest::CreateCheckpoint {
                    vm: menu.vm,
                    vm_name,
                }
            } else {
                OpRequest::Vm {
                    vm: menu.vm,
                    vm_name: vm_name.clone(),
                    op: item,
                }
            };
            start_operation(state, request, effects);
        }
        MenuKind::Checkpoint => {
            let Some(item) = CHECKPOINT_MENU.get(menu.cursor).copied() else {
                return;
            };
            let Some(vm_state) = vm_state_of(state, menu.vm) else {
                return;
            };
            let checkpoint_selected = menu.checkpoint.is_some();
            // The row may have greyed out since the menu opened: the menu is
            // the availability table (spec 3.4, anomaly 21).
            if !checkpoint_item_enabled(
                item,
                Some(vm_state),
                checkpoint_selected,
                state.ops.in_flight(menu.vm),
            ) {
                return;
            }
            // Create runs right away (spec 3.4) and needs no checkpoint: it is
            // the one row the menu offers without a selection (spec 3.8).
            if item == CheckpointMenuItem::CreateCheckpoint {
                start_operation(
                    state,
                    OpRequest::CreateCheckpoint {
                        vm: menu.vm,
                        vm_name,
                    },
                    effects,
                );
                return;
            }
            let Some((checkpoint, checkpoint_name)) = menu.checkpoint.clone() else {
                return;
            };
            let request = match item {
                CheckpointMenuItem::Restore => OpRequest::RestoreCheckpoint {
                    vm: menu.vm,
                    vm_name: vm_name.clone(),
                    checkpoint,
                    checkpoint_name: checkpoint_name.clone(),
                },
                CheckpointMenuItem::Remove => OpRequest::RemoveCheckpoint {
                    vm: menu.vm,
                    vm_name: vm_name.clone(),
                    checkpoint,
                    checkpoint_name: checkpoint_name.clone(),
                },
                // Handled above: Create never reaches the dialog (spec 3.4).
                CheckpointMenuItem::CreateCheckpoint => return,
            };
            if item.needs_confirm() {
                state.overlay = Overlay::Confirm(ConfirmState {
                    vm: menu.vm,
                    question: checkpoint_confirm_question(item, &vm_name, &checkpoint_name),
                    // Restore warns about the changes that are lost (spec 3.4).
                    note: (item == CheckpointMenuItem::Restore)
                        .then(|| "Changes made since the checkpoint will be lost.".to_string()),
                    request,
                });
                return;
            }
            start_operation(state, request, effects);
        }
    }
}

/// The `y` of a confirmation dialog: the only key that runs anything (spec 3.4).
fn confirm_yes(state: &mut State, effects: &mut Vec<Effect>) {
    let Overlay::Confirm(confirm) = state.overlay.clone() else {
        return;
    };
    start_operation(state, confirm.request, effects);
}

/// Marks the VM busy, closes the popup and asks the runner to perform the
/// operation (spec 3.5).
fn start_operation(state: &mut State, request: OpRequest, effects: &mut Vec<Effect>) {
    state
        .ops
        .start(request.vm(), op_label(&request).to_string());
    state.overlay = Overlay::None;
    effects.push(Effect::RunOperation(request));
}

/// What the spinner row shows while the operation runs (spec 3.5).
fn op_label(request: &OpRequest) -> &'static str {
    match request {
        OpRequest::Vm { op, .. } => match op {
            VmMenuItem::Start => "starting",
            VmMenuItem::Stop | VmMenuItem::StopForce => "stopping",
            VmMenuItem::Restart | VmMenuItem::RestartForce => "restarting",
            VmMenuItem::Save => "saving",
            VmMenuItem::Pause => "pausing",
            VmMenuItem::Resume => "resuming",
            VmMenuItem::CreateCheckpoint => "creating checkpoint",
        },
        OpRequest::CreateCheckpoint { .. } => "creating checkpoint",
        OpRequest::RestoreCheckpoint { .. } => "restoring checkpoint",
        OpRequest::RemoveCheckpoint { .. } => "removing checkpoint",
    }
}

/// Closes a menu or a dialog whose target just vanished from the list, and
/// says so unless the same VM was already announced as gone (design 5.6,
/// acceptance of spec 3.6).
fn close_overlay_of_a_vanished_vm(
    state: &mut State,
    target: Option<VmId>,
    name: Option<String>,
    selection_vanished: &Option<String>,
) {
    let Some(target) = target else {
        return;
    };
    if state.vm_list.name_of(target).is_some() {
        return;
    }
    state.overlay = Overlay::None;
    if let Some(name) = name {
        if selection_vanished.as_deref() != Some(name.as_str()) {
            state.status.show(
                StatusKind::Info,
                format!("VM '{name}' is no longer available"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyModifiers};
    use hypervctl_core::{Checkpoint, CheckpointId, CheckpointKind, MemoryConfig};
    use uuid::Uuid;

    use super::*;
    use crate::tui::store::menu::vm_item_enabled;

    fn vm(index: u128, name: &str, state: VmState) -> VmSummary {
        VmSummary {
            id: VmId::new(Uuid::from_u128(index << 96)),
            name: name.to_string(),
            state,
            cpu_usage_percent: None,
            memory_assigned_bytes: None,
            uptime: None,
            generation: Some(2),
        }
    }

    fn sample_state() -> State {
        // The list sorts by name, so web-01 has to be selected by hand.
        let mut state = State::new(
            vec![
                vm(1, "web-01", VmState::Running),
                vm(2, "web-02", VmState::Off),
                vm(3, "db-01", VmState::Saved),
                vm(4, "ghost", VmState::Unknown(42)),
            ],
            80,
            24,
        );
        state.vm_list.selected = Some(VmId::new(Uuid::from_u128(1 << 96)));
        state
            .vm_list
            .items
            .sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        state
    }

    fn open_vm_menu(state: &mut State) {
        let effects = reduce(state, Action::OpenMenu);
        assert!(effects.is_empty());
        assert!(matches!(state.overlay, Overlay::Menu(_)));
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn menu_cursor(state: &State) -> Option<usize> {
        match &state.overlay {
            Overlay::Menu(menu) => Some(menu.cursor),
            _ => None,
        }
    }

    #[test]
    fn the_menu_enables_only_what_the_specification_table_allows() {
        use VmMenuItem::*;
        let table = [
            (VmState::Off, Start, true),
            (VmState::Saved, Start, true),
            (VmState::Paused, Start, true),
            (VmState::Running, Start, false),
            (VmState::Running, Stop, true),
            (VmState::Off, Stop, false),
            (VmState::Running, StopForce, true),
            (VmState::Paused, StopForce, true),
            (VmState::Off, StopForce, false),
            (VmState::Running, Restart, true),
            (VmState::Running, RestartForce, true),
            (VmState::Off, Restart, false),
            (VmState::Running, Save, true),
            (VmState::Running, Pause, true),
            (VmState::Paused, Resume, true),
            (VmState::Off, Resume, false),
            (VmState::Running, CreateCheckpoint, true),
            (VmState::Off, CreateCheckpoint, true),
            (VmState::Saved, CreateCheckpoint, true),
            (VmState::Paused, CreateCheckpoint, true),
        ];
        for (vm_state, item, expected) in table {
            assert_eq!(
                vm_item_enabled(item, vm_state, false),
                expected,
                "{item:?} in {vm_state:?}"
            );
        }
        for item in VM_MENU {
            assert!(!vm_item_enabled(*item, VmState::Unknown(7), false));
            assert!(!vm_item_enabled(*item, VmState::Running, true));
        }
    }

    #[test]
    fn the_cursor_skips_disabled_rows() {
        let mut state = sample_state();
        open_vm_menu(&mut state);
        let stop = VM_MENU
            .iter()
            .position(|item| *item == VmMenuItem::Stop)
            .unwrap_or(0);
        assert_eq!(menu_cursor(&state), Some(stop));
        let _ = reduce(&mut state, Action::MenuUp);
        assert_eq!(menu_cursor(&state), Some(stop));
        let force = VM_MENU
            .iter()
            .position(|item| *item == VmMenuItem::StopForce)
            .unwrap_or(0);
        let _ = reduce(&mut state, Action::MenuDown);
        assert_eq!(menu_cursor(&state), Some(force));
    }

    #[test]
    fn a_force_stop_asks_before_it_runs_and_only_y_runs_it() {
        let mut state = sample_state();
        open_vm_menu(&mut state);
        let _ = reduce(&mut state, Action::MenuDown);
        let _ = reduce(&mut state, Action::MenuSelect);
        assert!(matches!(state.overlay, Overlay::Confirm(_)));
        let Overlay::Confirm(confirm) = state.overlay.clone() else {
            unreachable!("the dialog just opened");
        };
        assert!(confirm.question.contains("web-01"));
        for cancel in [
            Action::Input(key(KeyCode::Enter)),
            Action::Input(key(KeyCode::Esc)),
        ] {
            let mut cancelled = sample_state();
            open_vm_menu(&mut cancelled);
            let _ = reduce(&mut cancelled, Action::MenuDown);
            let _ = reduce(&mut cancelled, Action::MenuSelect);
            let _ = reduce(&mut cancelled, cancel);
            assert!(matches!(cancelled.overlay, Overlay::None));
            assert!(cancelled.ops.is_empty());
        }
        let _ = reduce(&mut state, Action::ConfirmYes);
        assert!(matches!(state.overlay, Overlay::None));
        assert!(state.ops.in_flight(VmId::new(Uuid::from_u128(1 << 96))));
    }

    #[test]
    fn a_vanished_target_closes_the_menu_and_is_announced_once() {
        let mut state = sample_state();
        open_vm_menu(&mut state);
        let remaining = vec![
            vm(2, "web-02", VmState::Off),
            vm(3, "db-01", VmState::Saved),
        ];
        let _ = reduce(&mut state, Action::VmListLoaded(Box::new(Ok(remaining))));
        assert!(matches!(state.overlay, Overlay::None));
        let text = state
            .status
            .current
            .as_ref()
            .map(|message| message.text.clone())
            .unwrap_or_default();
        assert!(text.contains("no longer available"), "{text}");
        assert!(text.contains("web-01"), "{text}");
        assert_eq!(
            state.vm_list.selected,
            Some(VmId::new(Uuid::from_u128(2 << 96)))
        );
    }

    #[test]
    fn create_checkpoint_runs_without_a_dialog() {
        let mut state = sample_state();
        open_vm_menu(&mut state);
        let target = VM_MENU
            .iter()
            .position(|item| *item == VmMenuItem::CreateCheckpoint)
            .unwrap_or(usize::MAX);
        while menu_cursor(&state) != Some(target) {
            let before = menu_cursor(&state).unwrap_or(0);
            let _ = reduce(&mut state, Action::MenuDown);
            if menu_cursor(&state) == Some(before) {
                break;
            }
        }
        assert_eq!(menu_cursor(&state), Some(target));
        let _ = reduce(&mut state, Action::MenuSelect);
        assert!(matches!(state.overlay, Overlay::None));
        assert!(state.ops.in_flight(VmId::new(Uuid::from_u128(1 << 96))));
    }

    #[test]
    fn help_comes_and_goes_without_losing_a_menu() {
        let mut state = sample_state();
        open_vm_menu(&mut state);
        let _ = reduce(&mut state, Action::ToggleHelp);
        assert!(matches!(state.overlay, Overlay::Menu(_)));
        let _ = reduce(&mut state, Action::CloseOverlay);
        assert!(matches!(state.overlay, Overlay::None));
        let _ = reduce(&mut state, Action::ToggleHelp);
        assert!(matches!(state.overlay, Overlay::Help));
        let _ = reduce(&mut state, Action::ToggleHelp);
        assert!(matches!(state.overlay, Overlay::None));
    }

    #[test]
    fn a_finished_operation_reports_and_asks_for_a_fresh_list() {
        let mut state = sample_state();
        let id = VmId::new(Uuid::from_u128(1 << 96));
        state.ops.start(id, "stopping".to_string());
        let at = Instant::now();
        let effects = reduce(
            &mut state,
            Action::OpFinished {
                vm: id,
                result: Ok("web-01: stopped".to_string()),
                at,
            },
        );
        assert!(!state.ops.in_flight(id));
        let text = state
            .status
            .current
            .as_ref()
            .map(|message| message.text.clone())
            .unwrap_or_default();
        assert_eq!(text, "web-01: stopped");
        assert!(state.status.needs_tick());
        assert!(effects.contains(&Effect::FetchVmList));
        let _ = reduce(
            &mut state,
            Action::UiTick(at + SUCCESS_LIFETIME + Duration::from_millis(1)),
        );
        assert!(state.status.current.is_none());
    }

    #[test]
    fn the_checkpoints_tab_reads_the_checkpoints_of_the_selected_vm() {
        let mut state = sample_state();
        let effects = reduce(&mut state, Action::SelectTab(DetailTab::Checkpoints));
        assert_eq!(
            effects,
            vec![Effect::FetchCheckpoints(VmId::new(Uuid::from_u128(
                1 << 96
            )))]
        );
        assert!(state.detail.checkpoints.in_flight);
        let again = reduce(&mut state, Action::SelectTab(DetailTab::Checkpoints));
        assert!(again.is_empty());
        let notified = reduce(
            &mut state,
            Action::CheckpointsChanged(VmId::new(Uuid::from_u128(1 << 96))),
        );
        assert!(notified.is_empty());
    }

    #[test]
    fn a_checkpoint_vanishing_is_announced() {
        let mut state = sample_state();
        let vm = VmId::new(Uuid::from_u128(1 << 96));
        let first = Checkpoint {
            id: CheckpointId::new(Uuid::from_u128(0x100 << 96)),
            name: "before-upgrade".to_string(),
            kind: CheckpointKind::Standard,
            created: None,
            parent_id: None,
        };
        let _ = reduce(&mut state, Action::SelectTab(DetailTab::Checkpoints));
        state.detail.checkpoints.in_flight = false;
        let _ = reduce(
            &mut state,
            Action::CheckpointsLoaded {
                vm,
                result: Box::new(Ok(vec![first])),
            },
        );
        assert_eq!(state.detail.checkpoints.items.len(), 1);
        state.detail.checkpoints.in_flight = false;
        let _ = reduce(
            &mut state,
            Action::CheckpointsLoaded {
                vm,
                result: Box::new(Ok(Vec::new())),
            },
        );
        let text = state
            .status
            .current
            .as_ref()
            .map(|message| message.text.clone())
            .unwrap_or_default();
        assert_eq!(text, "Checkpoint 'before-upgrade' is no longer available");
    }

    #[test]
    fn a_menu_on_the_checkpoints_tab_targets_the_selected_checkpoint() {
        let mut state = sample_state();
        state.focus = Focus::Detail;
        state.detail.tab = DetailTab::Checkpoints;
        let vm = VmId::new(Uuid::from_u128(1 << 96));
        let entry = Checkpoint {
            id: CheckpointId::new(Uuid::from_u128(0x100 << 96)),
            name: "before-upgrade".to_string(),
            kind: CheckpointKind::Standard,
            created: None,
            parent_id: None,
        };
        state.detail.checkpoints.apply(vm, vec![entry]);
        open_vm_menu(&mut state);
        let Overlay::Menu(menu) = &state.overlay else {
            unreachable!("the menu just opened");
        };
        assert_eq!(menu.kind, MenuKind::Checkpoint);
        assert_eq!(
            menu.checkpoint.as_ref().map(|(_, name)| name.as_str()),
            Some("before-upgrade")
        );
        // The cursor opens on Create; web-01 is Running, so Restore is grey
        // and the next row that can run is Remove (spec 3.4).
        let _ = reduce(&mut state, Action::MenuDown);
        let _ = reduce(&mut state, Action::MenuSelect);
        assert!(matches!(state.overlay, Overlay::Confirm(_)));
    }

    fn detail_of(vm: &VmSummary) -> VmDetail {
        VmDetail {
            summary: vm.clone(),
            processor_count: 2,
            memory: MemoryConfig {
                startup_bytes: 2 * 1024 * 1024 * 1024,
                dynamic_enabled: true,
                min_bytes: None,
                max_bytes: None,
            },
            disks: Vec::new(),
            nics: Vec::new(),
            checkpoint_count: 0,
            config_path: String::new(),
            integration_services: None,
            notes: None,
        }
    }

    #[test]
    fn a_late_detail_of_another_vm_is_dropped() {
        // Acceptance 10: a slow answer must not repaint the pane of the VM
        // that is on screen now.
        let mut state = sample_state();
        let web01 = VmId::new(Uuid::from_u128(1 << 96));
        let web02 = VmId::new(Uuid::from_u128(2 << 96));
        assert_eq!(state.vm_list.selected, Some(web01));
        let late = state
            .vm_list
            .items
            .iter()
            .find(|vm| vm.id == web02)
            .cloned()
            .unwrap_or_else(|| panic!("the fixture must contain web-02"));
        let _ = reduce(
            &mut state,
            Action::VmDetailLoaded {
                id: web02,
                result: Box::new(Ok(detail_of(&late))),
            },
        );
        assert!(state.detail.detail.is_none());
        let wanted = state
            .vm_list
            .items
            .iter()
            .find(|vm| vm.id == web01)
            .cloned()
            .unwrap_or_else(|| panic!("the fixture must contain web-01"));
        let _ = reduce(
            &mut state,
            Action::VmDetailLoaded {
                id: web01,
                result: Box::new(Ok(detail_of(&wanted))),
            },
        );
        assert_eq!(
            state.detail.detail.as_ref().map(|detail| detail.summary.id),
            Some(web01)
        );
    }

    #[test]
    fn a_tick_during_a_fetch_starts_no_second_one() {
        // Acceptance 11: ticks while a fetch is running neither stack up
        // fetches nor lose the fact that the list is behind.
        let mut state = sample_state();
        let first = reduce(&mut state, Action::RefreshTick);
        assert_eq!(first, vec![Effect::FetchVmList]);
        assert!(state.vm_list.refresh_in_flight);
        let second = reduce(&mut state, Action::RefreshTick);
        assert!(second.is_empty());
        let third = reduce(&mut state, Action::RefreshTick);
        assert!(third.is_empty());
        // A change announcement during the fetch is remembered, not doubled.
        let announced = reduce(&mut state, Action::BackendChanged);
        assert!(announced.is_empty());
        assert!(state.vm_list.refresh_again);
        // When the fetch lands, exactly one catch-up fetch starts.
        let items = state.vm_list.items.clone();
        let loaded = reduce(&mut state, Action::VmListLoaded(Box::new(Ok(items))));
        assert!(
            loaded
                .iter()
                .filter(|effect| **effect == Effect::FetchVmList)
                .count()
                == 1,
            "exactly one catch-up fetch, got {loaded:?}"
        );
        assert!(!state.vm_list.refresh_again);
        assert!(state.vm_list.refresh_in_flight);
    }

    #[test]
    fn the_filter_moves_the_selection_only_when_the_target_is_hidden() {
        // Anomaly 29: typing narrows the rows; a selection the filter still
        // shows stays put, a hidden one jumps to the first visible row (spec
        // 3.3), and the fetches stay whole-list.
        let mut state = sample_state();
        let web01 = VmId::new(Uuid::from_u128(1 << 96));
        let db01 = VmId::new(Uuid::from_u128(3 << 96));
        let _ = reduce(&mut state, Action::FilterStart);
        // web-01 is selected and "w" keeps it visible: nothing moves.
        let effects = reduce(&mut state, Action::FilterChar('w'));
        assert_eq!(state.vm_list.selected, Some(web01));
        assert_eq!(state.vm_list.visible, vec![2, 3], "web-01 and web-02");
        assert!(
            !effects.contains(&Effect::FetchVmList),
            "the filter fetches nothing, got {effects:?}"
        );
        // A filter that hides the selection moves it to the first visible row.
        let _ = reduce(&mut state, Action::FilterClear);
        let _ = reduce(&mut state, Action::FilterChar('d'));
        assert_eq!(state.vm_list.selected, Some(db01));
        assert_eq!(state.vm_list.visible, vec![0]);
        // Nothing matches: nothing is selected.
        let _ = reduce(&mut state, Action::FilterChar('z'));
        assert!(state.vm_list.visible.is_empty());
        assert!(state.vm_list.selected.is_none());
        // Esc closes the input and clears the filter (spec 3.3).
        let _ = reduce(&mut state, Action::Input(key(KeyCode::Esc)));
        assert!(!state.vm_list.filter.editing);
        assert!(state.vm_list.filter.text.is_empty());
        assert_eq!(state.vm_list.visible.len(), 4);
        // A refresh under a filter installs the whole list and the display
        // stays narrowed (spec 3.3: the fetching is never filtered).
        let _ = reduce(&mut state, Action::FilterChar('d'));
        let items = state.vm_list.items.clone();
        let _ = reduce(&mut state, Action::VmListLoaded(Box::new(Ok(items))));
        assert_eq!(state.vm_list.items.len(), 4);
        assert_eq!(state.vm_list.visible, vec![0]);
        assert_eq!(state.vm_list.selected, Some(db01));
    }

    #[test]
    fn a_key_while_the_filter_input_is_up_is_text_not_a_command() {
        // Anomaly 29: with the input up, q types a letter instead of
        // quitting, and the rows stay drivable (spec 3.3).
        let mut state = sample_state();
        let web02 = VmId::new(Uuid::from_u128(2 << 96));
        let _ = reduce(&mut state, Action::FilterStart);
        // Arrows still move the selection among the visible rows.
        let _ = reduce(&mut state, Action::Input(key(KeyCode::Down)));
        assert_eq!(state.vm_list.selected, Some(web02), "down from web-01");
        // q types a character: no quit, no overlay, no error.
        let effects = reduce(&mut state, Action::Input(key(KeyCode::Char('q'))));
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(state.vm_list.filter.text, "q");
        assert!(matches!(state.overlay, Overlay::None));
        // Enter applies the filter and keeps the text for the next edit.
        let _ = reduce(&mut state, Action::Input(key(KeyCode::Enter)));
        assert!(!state.vm_list.filter.editing);
        assert_eq!(state.vm_list.filter.text, "q");
        // Ctrl+U erases the text while the input stays up (spec 3.3).
        let _ = reduce(&mut state, Action::FilterStart);
        let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        let _ = reduce(&mut state, Action::Input(ctrl_u));
        assert!(state.vm_list.filter.text.is_empty());
        assert!(state.vm_list.filter.editing);
    }

    #[test]
    fn the_checkpoints_are_in_tree_order_and_the_menu_offers_create() {
        // Anomaly 30: the rows arrive in any order; the list lays them out as
        // a tree (spec 3.8) and the menu opens without a selection to offer
        // Create (spec 3.8).
        let mut state = sample_state();
        let _ = reduce(&mut state, Action::FocusDetail);
        let _ = reduce(&mut state, Action::SelectTab(DetailTab::Checkpoints));
        let vm = VmId::new(Uuid::from_u128(1 << 96));
        let root = Checkpoint {
            id: CheckpointId::new(Uuid::from_u128(0x100)),
            name: "root".to_string(),
            kind: CheckpointKind::Standard,
            created: Some(std::time::UNIX_EPOCH + Duration::from_secs(10)),
            parent_id: None,
        };
        let child = Checkpoint {
            id: CheckpointId::new(Uuid::from_u128(0x200)),
            name: "child".to_string(),
            kind: CheckpointKind::Standard,
            created: Some(std::time::UNIX_EPOCH + Duration::from_secs(20)),
            parent_id: Some(root.id),
        };
        // The provider hands them over in the wrong order on purpose.
        let vanished = state
            .detail
            .checkpoints
            .apply(vm, vec![child.clone(), root.clone()]);
        assert_eq!(vanished, None);
        let names: Vec<&str> = state
            .detail
            .checkpoints
            .items
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["root", "child"], "the child follows its parent");
        // The selection is kept by id through another scrambled reload.
        state.detail.checkpoints.selected = Some(child.id);
        let _ = state
            .detail
            .checkpoints
            .apply(vm, vec![child.clone(), root.clone()]);
        assert_eq!(state.detail.checkpoints.selected, Some(child.id));
        assert_eq!(state.detail.checkpoints.selected_index(), Some(1));
        // Without a selection the menu still opens; Create does not need one
        // and web-01 is Running, so it runs without a dialog (spec 3.4).
        state.detail.checkpoints.selected = None;
        let _ = reduce(&mut state, Action::OpenMenu);
        let Overlay::Menu(menu) = &state.overlay else {
            panic!("the checkpoint menu must open");
        };
        assert_eq!(menu.kind, MenuKind::Checkpoint);
        assert!(menu.checkpoint.is_none());
        assert_eq!(menu.cursor, 0, "Create is the first row that can run");
        let effects = reduce(&mut state, Action::MenuSelect);
        assert!(
            matches!(state.overlay, Overlay::None),
            "Create runs directly"
        );
        assert!(state.ops.in_flight(vm), "the operation started");
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::RunOperation(_))),
            "{effects:?}"
        );
    }
}
