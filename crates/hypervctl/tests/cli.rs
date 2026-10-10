//! Process-level tests of the binary: what leaves on stdout, what on stderr,
//! and which exit code it ends with (spec 2.10 and 2.11).

use std::process::{Command, Output};

/// Runs the binary against the fixture of the mock.
fn run(args: &[&str]) -> Output {
    run_with_backend(args, "mock")
}

fn run_with_backend(args: &[&str], backend: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hypervctl"))
        .args(args)
        .env("HYPERVCTL_BACKEND", backend)
        .env_remove("NO_COLOR")
        .output()
        .unwrap_or_else(|error| panic!("the binary must start for {args:?}: {error}"))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn status(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

#[test]
fn a_list_reaches_stdout_and_ends_with_a_newline() {
    let output = run(&["vm", "list"]);
    assert_eq!(status(&output), 0, "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.starts_with("NAME"), "{out:?}");
    assert!(out.contains("web-01"), "{out:?}");
    assert!(out.ends_with('\n'), "{out:?}");
    assert!(!out.contains('\x1b'), "{out:?}");
}

#[test]
fn json_and_csv_come_out_of_the_same_command() {
    let json = run(&["-o", "json", "vm", "list"]);
    assert_eq!(status(&json), 0, "{}", stderr(&json));
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&json))
        .unwrap_or_else(|error| panic!("the JSON must parse: {error}"));
    assert_eq!(parsed.as_array().map(Vec::len), Some(6));
    assert_eq!(parsed[0]["state"], "saved");

    let csv = run(&["-o", "csv", "vm", "list"]);
    assert_eq!(status(&csv), 0, "{}", stderr(&csv));
    let bytes = csv.stdout;
    assert!(bytes.starts_with(b"id,name,state"), "{bytes:?}");
    assert!(
        !bytes.starts_with(&[0xEF, 0xBB, 0xBF]),
        "CSV must carry no BOM"
    );
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(
        text.contains("00000001-0000-0000-0000-000000000000"),
        "{text:?}"
    );
    assert!(
        text.contains("\r\n"),
        "CSV uses CRLF line endings: {text:?}"
    );
}

#[test]
fn every_read_only_command_answers_with_zero() {
    for args in [
        vec!["vm", "list"],
        vec!["vm", "get", "web-01"],
        vec!["checkpoint", "list", "web-01"],
        vec!["switch", "list"],
        vec!["switch", "get", "test-external"],
        vec!["vhd", "list"],
        vec!["vhd", "get", r"C:\HyperV\web-01\disk.vhdx"],
    ] {
        let output = run(&args);
        assert_eq!(status(&output), 0, "{args:?} failed: {}", stderr(&output));
    }
}

#[test]
fn an_unknown_target_is_not_found_and_exits_with_three() {
    let output = run(&["vm", "get", "nope"]);
    assert_eq!(status(&output), 3);
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.starts_with("error: VM not found: 'nope'"), "{err:?}");
}

#[test]
fn an_error_stays_on_stderr_even_when_json_was_asked_for() {
    let output = run(&["-o", "json", "vm", "get", "nope"]);
    assert_eq!(status(&output), 3);
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(stderr(&output).contains("error:"), "{}", stderr(&output));
}

#[test]
fn an_ambiguous_name_lists_its_candidates() {
    let output = run(&["vm", "get", "dup"]);
    assert_eq!(status(&output), 3);
    let err = stderr(&output);
    assert!(err.contains("error: VM 'dup' is ambiguous"), "{err:?}");
    assert!(err.contains("  candidates:"), "{err:?}");
    assert!(
        err.contains("00000005-0000-0000-0000-000000000000  dup"),
        "{err:?}"
    );
    assert!(
        err.contains("00000006-0000-0000-0000-000000000000  dup"),
        "{err:?}"
    );
}

#[test]
fn an_ambiguous_switch_is_reported_the_same_way() {
    let output = run(&["switch", "get", "dup-switch"]);
    assert_eq!(status(&output), 3);
    let err = stderr(&output);
    assert!(
        err.contains("error: switch 'dup-switch' is ambiguous"),
        "{err:?}"
    );
    assert!(
        err.contains("00000202-0000-0000-0000-000000000000"),
        "{err:?}"
    );
    assert!(
        err.contains("00000203-0000-0000-0000-000000000000"),
        "{err:?}"
    );
}

#[test]
fn a_vhd_that_does_not_exist_is_not_found() {
    let output = run(&["vhd", "get", r"C:\nope.vhdx"]);
    assert_eq!(status(&output), 3);
    assert!(
        stderr(&output).contains("error: VHD not found"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_usage_mistake_exits_with_two() {
    // A missing target is a usage mistake.
    let missing = run(&["vm", "start"]);
    assert_eq!(status(&missing), 2);
    assert!(stdout(&missing).is_empty(), "{}", stdout(&missing));

    // The two waiting flags cannot both be given (spec 2.7).
    let both = run(&["vm", "start", "web-01", "--no-wait", "--timeout", "5"]);
    assert_eq!(status(&both), 2);
    assert!(stdout(&both).is_empty(), "{}", stdout(&both));
}

#[test]
fn colour_is_only_forced_on_demand() {
    let plain = run(&["vm", "list"]);
    assert!(!plain.stdout.contains(&0x1b), "{:?}", plain.stdout);

    let coloured = run(&["--color", "always", "vm", "list"]);
    assert_eq!(status(&coloured), 0, "{}", stderr(&coloured));
    let green = [0x1b, b'[', b'3', b'2', b'm'];
    assert!(
        coloured
            .stdout
            .windows(green.len())
            .any(|window| window == green),
        "{:?}",
        coloured.stdout
    );
}

#[test]
fn no_trunc_prints_the_whole_identifier() {
    let output = run(&["--no-trunc", "vm", "list"]);
    assert_eq!(status(&output), 0, "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("00000001-0000-0000-0000-000000000000"),
        "{out:?}"
    );
}

#[test]
fn no_header_drops_the_first_line() {
    let output = run(&["--no-header", "vm", "list"]);
    assert_eq!(status(&output), 0, "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.starts_with("db-01"), "{out:?}");
    assert!(!out.contains("NAME"), "{out:?}");
}

#[test]
fn a_name_is_never_silently_guessed() {
    let output = run(&["checkpoint", "list", "nope"]);
    assert_eq!(status(&output), 3);
    assert!(
        stderr(&output).contains("error: VM not found: 'nope'"),
        "{}",
        stderr(&output)
    );
}

#[cfg(windows)]
#[test]
fn the_real_backend_is_refused_until_it_is_implemented() {
    let output = run_with_backend(&["vm", "list"], "wmi");
    assert_eq!(status(&output), 1);
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.contains("error: backend error:"), "{err:?}");
    assert!(err.contains("HYPERVCTL_BACKEND=mock"), "{err:?}");
    assert!(err.contains("hint:"), "{err:?}");
}

#[test]
fn an_unknown_backend_value_is_reported() {
    let output = run_with_backend(&["vm", "list"], "kvm");
    assert_eq!(status(&output), 1);
    let err = stderr(&output);
    assert!(
        err.contains("unknown HYPERVCTL_BACKEND value: kvm"),
        "{err:?}"
    );
}

// --- M5: operations (spec 2.3, 2.4, 2.6 to 2.11) ---

#[test]
fn an_operation_that_is_already_true_reports_itself_and_succeeds() {
    let started = run(&["vm", "start", "web-01"]);
    assert_eq!(status(&started), 0, "{}", stderr(&started));
    assert!(stdout(&started).is_empty(), "{}", stdout(&started));
    assert!(
        stderr(&started).contains("web-01: already running"),
        "{:?}",
        stderr(&started)
    );

    let stopped = run(&["vm", "stop", "web-02"]);
    assert_eq!(status(&stopped), 0, "{}", stderr(&stopped));
    assert!(
        stderr(&stopped).contains("web-02: already stopped"),
        "{:?}",
        stderr(&stopped)
    );

    let saved = run(&["vm", "save", "db-01"]);
    assert_eq!(status(&saved), 0, "{}", stderr(&saved));
    assert!(
        stderr(&saved).contains("db-01: already saved"),
        "{:?}",
        stderr(&saved)
    );
}

#[test]
fn an_operation_reaches_its_goal_and_says_so() {
    let output = run(&["vm", "start", "web-02"]);
    assert_eq!(status(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("web-02: started"),
        "{:?}",
        stderr(&output)
    );
}

#[test]
fn one_failure_among_several_targets_exits_with_one() {
    let output = run(&["vm", "start", "web-01", "nope"]);
    assert_eq!(status(&output), 1);
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.contains("web-01: already running"), "{err:?}");
    assert!(err.contains("error: nope: VM not found: 'nope'"), "{err:?}");
}

#[test]
fn an_unknown_target_of_an_operation_is_not_found_and_exits_with_three() {
    let output = run(&["vm", "start", "nope"]);
    assert_eq!(status(&output), 3);
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.contains("error: VM not found: 'nope'"), "{err:?}");
}

#[test]
fn a_destructive_verb_needs_a_terminal_or_the_yes_flag() {
    let output = run(&["vm", "stop", "--force", "web-01"]);
    assert_eq!(status(&output), 2);
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.contains("error:"), "{err:?}");
    assert!(err.contains("-y"), "{err:?}");
}

#[test]
fn the_yes_flag_lets_a_destructive_verb_through() {
    let output = run(&["-y", "vm", "stop", "--force", "web-01"]);
    assert_eq!(status(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("web-01: stopped"),
        "{:?}",
        stderr(&output)
    );
}

#[test]
fn no_wait_submits_and_says_so() {
    let output = run(&["vm", "start", "web-02", "--no-wait"]);
    assert_eq!(status(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("web-02: submitted"),
        "{:?}",
        stderr(&output)
    );
}

#[test]
fn operation_rows_come_out_as_json_and_csv() {
    let json = run(&["-o", "json", "vm", "start", "web-02"]);
    assert_eq!(status(&json), 0, "{}", stderr(&json));
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&json))
        .unwrap_or_else(|error| panic!("the JSON must parse: {error}"));
    assert_eq!(parsed[0]["name"], "web-02");
    assert_eq!(parsed[0]["status"], "succeeded");
    assert_eq!(parsed[0]["id"], "00000002-0000-0000-0000-000000000000");
    assert!(parsed[0]["error"].is_null(), "{parsed:?}");
    assert!(
        !stderr(&json).contains(": started"),
        "result lines belong to a terminal, not to a machine-readable stream: {:?}",
        stderr(&json)
    );

    let csv = run(&["-o", "csv", "vm", "start", "web-02"]);
    assert_eq!(status(&csv), 0, "{}", stderr(&csv));
    let text = String::from_utf8_lossy(&csv.stdout).into_owned();
    assert!(text.starts_with("id,name,status"), "{text:?}");
    assert!(
        text.contains("00000002-0000-0000-0000-000000000000,web-02,succeeded"),
        "{text:?}"
    );
    assert!(!stderr(&csv).contains(": started"), "{:?}", stderr(&csv));
}

#[test]
fn a_checkpoint_operation_shows_the_checkpoint_it_acted_on() {
    let output = run(&["-y", "checkpoint", "remove", "web-01", "nightly"]);
    assert_eq!(status(&output), 0, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("nightly: removed"),
        "{:?}",
        stderr(&output)
    );

    let json = run(&[
        "-y",
        "-o",
        "json",
        "checkpoint",
        "remove",
        "web-01",
        "nightly",
    ]);
    assert_eq!(status(&json), 0, "{}", stderr(&json));
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&json))
        .unwrap_or_else(|error| panic!("the JSON must parse: {error}"));
    assert_eq!(parsed[0]["name"], "nightly");
    assert_eq!(parsed[0]["status"], "succeeded");
    assert_eq!(parsed[0]["id"], "00000101-0000-0000-0000-000000000000");
}

#[test]
fn a_checkpoint_of_a_running_vm_cannot_be_restored() {
    let output = run(&["-y", "checkpoint", "restore", "web-01", "before-upgrade"]);
    assert_eq!(status(&output), 1);
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.contains("error:"), "{err:?}");
    assert!(err.contains("not valid in state Running"), "{err:?}");
}

#[test]
fn an_unknown_checkpoint_is_not_found_and_exits_with_three() {
    let output = run(&["-y", "checkpoint", "restore", "web-01", "nope"]);
    assert_eq!(status(&output), 3);
    let err = stderr(&output);
    assert!(err.contains("error:"), "{err:?}");
}

#[test]
fn an_operation_verb_still_takes_the_global_options_after_itself() {
    let output = run(&["vm", "start", "web-02", "-o", "json"]);
    assert_eq!(status(&output), 0, "{}", stderr(&output));
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output))
        .unwrap_or_else(|error| panic!("the JSON must parse: {error}"));
    assert_eq!(parsed[0]["status"], "succeeded");
}
