//! Snapshot tests of the drawn frames (design 5.9): a golden file per
//! scenario, compared after the usual end-of-line cleanup, and rewritten when
//! `HYPERVCTL_BLESS=1` is set.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hypervctl_core::{
    Checkpoint, CheckpointId, CheckpointKind, MemoryConfig, VmDetail, VmId, VmState, VmSummary,
};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use uuid::Uuid;

use crate::tui::store::{Action, DetailTab, Focus, Overlay, State};
use crate::tui::ui;

/// One VM of the snapshots, in the same shape the mock fixture uses.
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

fn detail_of(summary: VmSummary) -> VmDetail {
    VmDetail {
        summary,
        processor_count: 2,
        memory: MemoryConfig {
            startup_bytes: 2 * 1024 * 1024 * 1024,
            dynamic_enabled: true,
            min_bytes: Some(512 * 1024 * 1024),
            max_bytes: Some(4 * 1024 * 1024 * 1024),
        },
        disks: Vec::new(),
        nics: Vec::new(),
        checkpoint_count: 1,
        config_path: r"C:\ProgramData\Microsoft\Windows\Hyper-V\web-01.xml".to_string(),
        integration_services: None,
        notes: None,
    }
}

fn sample_items() -> Vec<VmSummary> {
    vec![
        vm(1, "web-01", VmState::Running),
        vm(2, "web-02", VmState::Off),
        vm(3, "db-01", VmState::Saved),
        vm(4, "ghost", VmState::Unknown(42)),
    ]
}

fn sample_state(width: u16, height: u16) -> State {
    let items = sample_items();
    let mut state = State::new(items.clone(), width, height);
    let selected = state.vm_list.selected;
    if let Some(id) = selected {
        let summary = items.into_iter().find(|vm| vm.id == id).unwrap_or_else(|| {
            unreachable!("the selection is always one of the items just installed")
        });
        state.detail.show(id, detail_of(summary));
    }
    state
}

/// A list of twelve VMs, so that the `rows` label has something to say.
fn many_vms_state(width: u16, height: u16) -> State {
    let items: Vec<VmSummary> = (1..=12)
        .map(|index| {
            let number = format!("vm-{index:02}");
            vm(index, &number, VmState::Off)
        })
        .collect();
    let mut state = State::new(items.clone(), width, height);
    if let Some(id) = state.vm_list.selected {
        let summary = items.into_iter().find(|vm| vm.id == id).unwrap_or_else(|| {
            unreachable!("the selection is always one of the items just installed")
        });
        state.detail.show(id, detail_of(summary));
    }
    state
}

/// Draws `state` on a fake terminal and returns the rows of the buffer.
fn frame(state: &State, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height))
        .unwrap_or_else(|error| panic!("a test terminal is in memory and cannot fail: {error}"));
    terminal
        .draw(|frame| ui::draw(frame, state))
        .unwrap_or_else(|error| panic!("drawing to a test terminal cannot fail: {error}"));
    let buffer = terminal.backend().buffer();
    let mut rows = Vec::new();
    for y in 0..height {
        let mut row = String::new();
        for x in 0..width {
            let cell = buffer
                .cell((x, y))
                .unwrap_or_else(|| panic!("({x},{y}) is inside the buffer"));
            row.push_str(cell.symbol());
        }
        rows.push(row.trim_end().to_string());
    }
    rows
}

fn compare(name: &str, rows: &[String]) {
    let normalized = rows.join("\n") + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("HYPERVCTL_BLESS").is_some() {
        let text = normalized.replace("\r\n", "\n");
        let _ = std::fs::create_dir_all(
            path.parent()
                .unwrap_or_else(|| unreachable!("the snapshot directory has a parent")),
        );
        std::fs::write(&path, text)
            .unwrap_or_else(|error| panic!("writing {} failed: {error}", path.display()));
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "reading {} failed: {error}; run the tests with HYPERVCTL_BLESS=1 to create it",
            path.display()
        )
    });
    let expected = expected.replace("\r\n", "\n");
    assert_eq!(
        expected, normalized,
        "the snapshot {name} changed; run with HYPERVCTL_BLESS=1 to accept the new frame"
    );
}

fn reduce(state: &mut State, action: Action) {
    let _ = crate::tui::store::reduce(state, action);
}

/// Moves the list selection down `steps` times (and the detail with it).
fn select_down(state: &mut State, steps: usize) {
    for _ in 0..steps {
        reduce(state, Action::SelectNext);
    }
}

#[test]
fn the_whole_layout_at_the_minimum_size() {
    let state = sample_state(80, 24);
    let rows = frame(&state, 80, 24);
    assert!(rows[0].contains("VMs (4)"), "{:?}", rows[0]);
    // NAME is the column header, the first inner row under the top border.
    assert!(rows[1].contains("NAME"), "{:?}", rows[1]);
    compare("layout-80x24", &rows);
}

#[test]
fn a_terminal_below_the_minimum_gets_one_notice() {
    let state = sample_state(40, 10);
    compare("too-small-40x10", &frame(&state, 40, 10));
}

/// Anomaly 28: the menu sits inside the detail pane; the list keeps showing.
#[test]
fn the_action_menu_sits_inside_the_detail_pane() {
    let mut state = sample_state(80, 24);
    reduce(&mut state, Action::OpenMenu);
    let rows = frame(&state, 80, 24);
    assert!(
        rows.iter().any(|row| row.contains("Actions · db-01")),
        "{rows:?}"
    );
    // The list header survives above the popup.
    assert!(rows[1].contains("NAME"), "{:?}", rows[1]);
    compare("menu-80x24", &rows);
}

/// The confirm dialog asks the question of the specification and names the
/// one key that runs it (spec 3.4).
#[test]
fn the_confirmation_dialog_sits_inside_the_detail_pane() {
    let mut state = sample_state(80, 24);
    // web-01 is Running: Stop (force) needs a confirmation.
    select_down(&mut state, 2);
    reduce(&mut state, Action::OpenMenu);
    reduce(&mut state, Action::MenuDown);
    reduce(&mut state, Action::MenuSelect);
    match &state.overlay {
        Overlay::Confirm(confirm) => {
            assert_eq!(confirm.question, "Force-stop web-01?");
            assert_eq!(confirm.note, None);
        }
        other => panic!("a confirmation must be open, not {other:?}"),
    }
    let rows = frame(&state, 80, 24);
    assert!(rows.iter().any(|row| row.contains("Force-stop web-01?")));
    assert!(
        rows.iter()
            .any(|row| row.contains("y run") && row.contains("any other key")),
        "{rows:?}"
    );
    compare("confirm-80x24", &rows);
}

/// Restore warns about the changes that are lost, on a second line (spec 3.4).
#[test]
fn the_restore_dialog_warns_about_the_changes() {
    let mut state = sample_state(80, 24);
    // web-02 is Off: checkpoints may be restored there.
    select_down(&mut state, 3);
    reduce(&mut state, Action::SelectTab(DetailTab::Checkpoints));
    let vm = state
        .vm_list
        .selected
        .unwrap_or_else(|| unreachable!("a VM is always selected in the sample"));
    let checkpoint = Checkpoint {
        id: CheckpointId::new(Uuid::from_u128(0xC0FFEE_u128 << 96)),
        name: "ckpt-1".to_string(),
        kind: CheckpointKind::Standard,
        created: None,
        parent_id: None,
    };
    let _ = state.detail.checkpoints.apply(vm, vec![checkpoint]);
    state.detail.checkpoints.in_flight = false;
    // The checkpoint menu only opens from the detail pane (spec 3.3).
    reduce(&mut state, Action::FocusDetail);
    reduce(&mut state, Action::OpenMenu);
    reduce(&mut state, Action::MenuSelect);
    match &state.overlay {
        Overlay::Confirm(confirm) => {
            assert_eq!(confirm.question, "Restore 'ckpt-1' of web-02?");
            assert_eq!(
                confirm.note.as_deref(),
                Some("Changes made since the checkpoint will be lost.")
            );
        }
        other => panic!("a confirmation must be open, not {other:?}"),
    }
    compare("confirm-restore-80x24", &frame(&state, 80, 24));
}

#[test]
fn the_help_covers_the_panes() {
    let mut state = sample_state(80, 24);
    reduce(&mut state, Action::ToggleHelp);
    compare("help-80x24", &frame(&state, 80, 24));
}

/// The help scrolls with its own keys (design 5.11).
#[test]
fn the_help_scrolls_with_its_keys() {
    let mut state = sample_state(80, 24);
    reduce(&mut state, Action::ToggleHelp);
    for _ in 0..5 {
        reduce(&mut state, Action::ScrollDown);
    }
    compare("help-scrolled-80x24", &frame(&state, 80, 24));
}

#[test]
fn the_checkpoints_tab_of_a_vm_without_checkpoints() {
    let mut state = sample_state(80, 24);
    reduce(&mut state, Action::SelectTab(DetailTab::Checkpoints));
    // Pretend the fetch already answered with nothing.
    state.detail.checkpoints.in_flight = false;
    compare("checkpoints-empty-80x24", &frame(&state, 80, 24));
}

/// Anomaly 33: the spinner takes the marker column; every row keeps the same
/// column positions.
#[test]
fn a_vm_with_an_operation_shows_a_spinner() {
    let mut state = sample_state(80, 24);
    let selected = state
        .vm_list
        .selected
        .unwrap_or_else(|| unreachable!("a VM is always selected in the sample"));
    state.ops.start(selected, "stopping".to_string());
    let rows = frame(&state, 80, 24);
    // The busy row carries a spinner in the marker column (anomaly 33); the
    // label itself only shows in the status bar.
    assert!(
        ["|", "/", "-", "\\"]
            .iter()
            .any(|spinner| rows[2].contains(&format!("{spinner} db-01"))),
        "{:?}",
        rows[2]
    );
    // The status bar carries the operation by name.
    assert!(
        rows[rows.len() - 1].contains("db-01: stopping..."),
        "{:?}",
        rows[rows.len() - 1]
    );
    // Every item row is as wide as the pane's inner width, spinner or not
    // (anomaly 33): compare a busy row against a plain one.
    let busy_width = rows[2].len();
    assert_eq!(rows[3].len(), busy_width, "{rows:?}");
    compare("spinner-80x24", &rows);
}

/// The detail pane gets the thick frame when it has the focus (spec 3.2).
#[test]
fn the_focused_pane_gets_a_thick_frame() {
    let mut state = sample_state(80, 24);
    reduce(&mut state, Action::FocusDetail);
    assert_eq!(state.focus, Focus::Detail);
    let rows = frame(&state, 80, 24);
    // The detail pane's top-left corner is thick.
    assert!(
        rows[state.ui.layout.list.height as usize].starts_with('┏'),
        "{rows:?}"
    );
    compare("detail-focus-80x24", &rows);
}

/// Twelve VMs: the list scrolls and says which rows are on screen (spec 3.2).
#[test]
fn a_long_list_shows_which_rows_are_on_screen() {
    let mut state = many_vms_state(80, 24);
    // The range label sits on the list pane's bottom border.
    let bottom = state.ui.layout.list.height.saturating_sub(1) as usize;
    let rows = frame(&state, 80, 24);
    assert!(
        rows[bottom].contains("rows 1-6 of 12"),
        "{:?}",
        rows[bottom]
    );
    reduce(&mut state, Action::SelectLast);
    let rows = frame(&state, 80, 24);
    assert!(
        rows[bottom].contains("rows 7-12 of 12"),
        "{:?}",
        rows[bottom]
    );
    assert!(rows[1 + 6].contains("vm-12"), "{:?}", rows[1 + 6]);
    compare("rows-12-80x24", &rows);
}

/// The detail body scrolls and the pane says which lines are shown (spec 3.2).
#[test]
fn the_detail_body_scrolls_and_reports_its_lines() {
    let mut state = sample_state(80, 24);
    reduce(&mut state, Action::FocusDetail);
    reduce(&mut state, Action::ScrollBottom);
    let rows = frame(&state, 80, 24);
    assert!(
        rows[state.ui.layout.list.height as usize + state.ui.layout.detail.height as usize - 1]
            .contains("lines "),
        "{rows:?}"
    );
    compare("detail-scroll-80x24", &rows);
}

/// A menu on a VM whose state is unknown says so and greys everything (spec
/// 3.4).
#[test]
fn a_menu_on_an_unknown_vm_says_so_and_greys_everything() {
    let mut state = sample_state(80, 24);
    select_down(&mut state, 1); // ghost, Unknown(42)
    reduce(&mut state, Action::OpenMenu);
    assert!(matches!(state.overlay, Overlay::Menu(_)));
    let rows = frame(&state, 80, 24);
    assert!(
        rows.iter().any(|row| row.contains("VM state is unknown")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("Actions · ghost")),
        "{rows:?}"
    );
    compare("menu-unknown-80x24", &rows);
}

/// The list title carries `[mock]` and the age of a failed refresh (spec 3.2),
/// and the bar shows the failure with its `! ` prefix (spec 3.5).
#[test]
fn a_mock_backend_and_a_stale_list_are_announced() {
    let mut state = sample_state(80, 24);
    state.ui.mock = true;
    let now = state.ui.now;
    state.vm_list.stale = Some("refresh failed".to_string());
    state.vm_list.stale_since = Some(now - Duration::from_secs(12));
    let rows = frame(&state, 80, 24);
    assert!(rows[0].contains("[mock] stale 12s"), "{:?}", rows[0]);
    assert!(
        rows[rows.len() - 1].contains("! refresh failed"),
        "{:?}",
        rows[rows.len() - 1]
    );
    compare("mock-stale-80x24", &rows);
}

/// The in-flight map is keyed by VM; the clock of the `stale` age is the one
/// the `UiTick` advances (design 5.2).
#[test]
fn the_stale_age_follows_the_ui_clock() {
    let mut state = sample_state(80, 24);
    let since = Instant::now() - Duration::from_secs(3);
    state.vm_list.stale = Some("refresh failed".to_string());
    state.vm_list.stale_since = Some(since);
    reduce(&mut state, Action::UiTick(since + Duration::from_secs(9)));
    assert_eq!(state.ui.now, since + Duration::from_secs(9));
    assert!(state.needs_ui_tick(), "the age keeps ticking");
}
