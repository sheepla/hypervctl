//! Golden files for the output contract (design 4.4): every read-only command,
//! in every format, rendered against the fixture of the mock.
//!
//! Set `HYPERVCTL_BLESS=1` to rewrite the files after an intended change:
//!
//! ```text
//! HYPERVCTL_BLESS=1 cargo test -p hypervctl golden
//! ```

use std::path::{Path, PathBuf};

use clap::Parser;

use hypervctl_backend_mock::MockBackend;
use hypervctl_core::{BackendActor, Operations};

use crate::cli::args::Cli;
use crate::cli::commands;

/// Runs one command against the fixture and returns exactly what it writes to
/// stdout.
fn render(args: &[&str]) -> String {
    let mut argv = vec!["hypervctl"];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(&argv)
        .unwrap_or_else(|error| panic!("the arguments {args:?} must parse: {error}"));
    // Not a terminal, no known width, `NO_COLOR` unset: the flags decide alone.
    let opts = super::build(&cli.global, false, None, true);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| panic!("the test runtime must start: {error}"));
    runtime
        .block_on(async {
            let ops = Operations::new(BackendActor::spawn(MockBackend::with_fixture));
            commands::run(&cli.resource, &ops, &opts).await
        })
        .unwrap_or_else(|error| panic!("{args:?} must render: {error}"))
}

fn blessed() -> bool {
    match std::env::var_os("HYPERVCTL_BLESS") {
        Some(value) => !value.is_empty() && value != "0",
        None => false,
    }
}

/// Compares the output with the checked-in file, or writes it when blessing.
fn golden(name: &str, args: &[&str]) {
    let produced = render(args);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name);

    if blessed() {
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(directory)
            .unwrap_or_else(|error| panic!("creating {directory:?} must succeed: {error}"));
        std::fs::write(&path, &produced)
            .unwrap_or_else(|error| panic!("writing {path:?} must succeed: {error}"));
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "the golden file {name} is missing; run \
             `HYPERVCTL_BLESS=1 cargo test -p hypervctl golden`: {error}"
        )
    });
    assert_eq!(
        normalize(&expected),
        normalize(&produced),
        "the output of {args:?} changed; run \
         `HYPERVCTL_BLESS=1 cargo test -p hypervctl golden` to accept it"
    );
}

/// The CSV files are written with CRLF, and a checkout may translate line
/// endings either way: compare line by line.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[test]
fn vm_list_as_a_table() {
    golden("vm-list-table.txt", &["vm", "list"]);
}

#[test]
fn vm_list_as_json() {
    golden("vm-list-json.txt", &["-o", "json", "vm", "list"]);
}

#[test]
fn vm_list_as_csv() {
    golden("vm-list-csv.txt", &["-o", "csv", "vm", "list"]);
}

#[test]
fn vm_list_without_a_header() {
    golden("vm-list-no-header.txt", &["--no-header", "vm", "list"]);
}

#[test]
fn vm_list_with_full_identifiers() {
    golden("vm-list-no-trunc.txt", &["--no-trunc", "vm", "list"]);
}

#[test]
fn vm_list_with_forced_colour() {
    golden("vm-list-colour.txt", &["--color", "always", "vm", "list"]);
}

#[test]
fn vm_get_as_a_table() {
    golden("vm-get-table.txt", &["vm", "get", "web-01"]);
}

#[test]
fn vm_get_as_json() {
    golden("vm-get-json.txt", &["-o", "json", "vm", "get", "web-01"]);
}

#[test]
fn vm_get_as_csv() {
    golden("vm-get-csv.txt", &["-o", "csv", "vm", "get", "web-01"]);
}

#[test]
fn checkpoint_list_as_a_table() {
    golden(
        "checkpoint-list-table.txt",
        &["checkpoint", "list", "web-01"],
    );
}

#[test]
fn checkpoint_list_as_json() {
    golden(
        "checkpoint-list-json.txt",
        &["-o", "json", "checkpoint", "list", "web-01"],
    );
}

#[test]
fn checkpoint_list_as_csv() {
    golden(
        "checkpoint-list-csv.txt",
        &["-o", "csv", "checkpoint", "list", "web-01"],
    );
}

#[test]
fn switch_list_as_a_table() {
    golden("switch-list-table.txt", &["switch", "list"]);
}

#[test]
fn switch_list_as_json() {
    golden("switch-list-json.txt", &["-o", "json", "switch", "list"]);
}

#[test]
fn switch_list_as_csv() {
    golden("switch-list-csv.txt", &["-o", "csv", "switch", "list"]);
}

#[test]
fn switch_get_as_a_table() {
    golden("switch-get-table.txt", &["switch", "get", "test-external"]);
}

#[test]
fn switch_get_as_json() {
    golden(
        "switch-get-json.txt",
        &["-o", "json", "switch", "get", "test-external"],
    );
}

#[test]
fn switch_get_as_csv() {
    golden(
        "switch-get-csv.txt",
        &["-o", "csv", "switch", "get", "test-external"],
    );
}

#[test]
fn vhd_list_as_a_table() {
    golden("vhd-list-table.txt", &["vhd", "list"]);
}

#[test]
fn vhd_list_as_json() {
    golden("vhd-list-json.txt", &["-o", "json", "vhd", "list"]);
}

#[test]
fn vhd_list_as_csv() {
    golden("vhd-list-csv.txt", &["-o", "csv", "vhd", "list"]);
}

#[test]
fn vhd_list_filtered_by_vm() {
    golden("vhd-list-vm-table.txt", &["vhd", "list", "--vm", "web-01"]);
}

#[test]
fn vhd_get_as_a_table() {
    golden(
        "vhd-get-table.txt",
        &["vhd", "get", r"C:\HyperV\web-01\disk.vhdx"],
    );
}

#[test]
fn vhd_get_as_json() {
    golden(
        "vhd-get-json.txt",
        &["-o", "json", "vhd", "get", r"C:\HyperV\web-01\disk.vhdx"],
    );
}

#[test]
fn vhd_get_as_csv() {
    golden(
        "vhd-get-csv.txt",
        &["-o", "csv", "vhd", "get", r"C:\HyperV\web-01\disk.vhdx"],
    );
}
