//! Snapshot tests of the drawn frames (design 5.9): a golden file per
//! scenario, compared after the usual end-of-line cleanup, and rewritten when
//! `HYPERVCTL_BLESS=1` is set.

use std::path::PathBuf;

use hypervctl_core::{MemoryConfig, VmDetail, VmId, VmState, VmSummary};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use uuid::Uuid;

use crate::tui::store::State;
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

fn sample_state(width: u16, height: u16) -> State {
    let items = vec![
        vm(1, "web-01", VmState::Running),
        vm(2, "web-02", VmState::Off),
        vm(3, "db-01", VmState::Saved),
        vm(4, "ghost", VmState::Unknown(42)),
    ];
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

#[test]
fn the_whole_layout_at_the_minimum_size() {
    let state = sample_state(80, 24);
    compare("layout-80x24", &frame(&state, 80, 24));
}

#[test]
fn a_terminal_below_the_minimum_gets_one_notice() {
    let state = sample_state(40, 10);
    compare("too-small-40x10", &frame(&state, 40, 10));
}

#[test]
fn the_action_menu_covers_the_panes() {
    let mut state = sample_state(80, 24);
    let _ = crate::tui::store::reduce(&mut state, crate::tui::store::Action::OpenMenu);
    compare("menu-80x24", &frame(&state, 80, 24));
}

#[test]
fn the_confirmation_dialog_covers_the_panes() {
    let mut state = sample_state(80, 24);
    let _ = crate::tui::store::reduce(&mut state, crate::tui::store::Action::OpenMenu);
    let _ = crate::tui::store::reduce(&mut state, crate::tui::store::Action::MenuDown);
    let _ = crate::tui::store::reduce(&mut state, crate::tui::store::Action::MenuSelect);
    compare("confirm-80x24", &frame(&state, 80, 24));
}

#[test]
fn the_help_covers_the_panes() {
    let mut state = sample_state(80, 24);
    let _ = crate::tui::store::reduce(&mut state, crate::tui::store::Action::ToggleHelp);
    compare("help-80x24", &frame(&state, 80, 24));
}

#[test]
fn the_checkpoints_tab_of_a_vm_without_checkpoints() {
    let mut state = sample_state(80, 24);
    let _ = crate::tui::store::reduce(
        &mut state,
        crate::tui::store::Action::SelectTab(crate::tui::store::DetailTab::Checkpoints),
    );
    // Pretend the fetch already answered with nothing.
    state.detail.checkpoints.in_flight = false;
    compare("checkpoints-empty-80x24", &frame(&state, 80, 24));
}

#[test]
fn a_vm_with_an_operation_shows_a_spinner() {
    let mut state = sample_state(80, 24);
    let selected = state.vm_list.selected;
    if let Some(id) = selected {
        state.ops.start(id, "stopping".to_string());
    }
    compare("spinner-80x24", &frame(&state, 80, 24));
}
