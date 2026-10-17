# hypervctl

A command line and full-screen terminal interface for Hyper-V, aimed at
application development and testing on a local Hyper-V host: manage virtual
machines, checkpoints, switches and virtual hard disks without opening
Hyper-V Manager or memorising PowerShell.

**Status: early but working on real hardware.** The CLI and the TUI run
against a real Hyper-V host through WMI on Windows. A mock backend is bundled
for development and for trying the tool on machines without Hyper-V.

## Install

The project is a Rust workspace. Building it needs a Rust toolchain
([rustup](https://rustup.rs/) recommended).

```console
git clone https://github.com/sheepla/hypervctl
cd hypervctl
cargo build --release
```

The binary is written to `target/release/hypervctl`. To install it into your
`PATH`:

```console
cargo install --path crates/hypervctl
```

## Usage

Every command has the shape:

```console
hypervctl [global options] <resource> <verb> [targets...] [options]
```

Resources are `vm`, `checkpoint`, `switch`, `vhd`, and `tui`.

```console
# List the virtual machines
hypervctl vm list

# The same list as JSON or CSV
hypervctl -o json vm list
hypervctl -o csv vm list

# One machine in full
hypervctl vm get web-01

# Power operations (one or more targets)
hypervctl vm start web-01
hypervctl vm stop web-01
hypervctl vm stop --force web-01      # asks for confirmation
hypervctl vm restart web-01
hypervctl vm save web-01
hypervctl vm pause web-01
hypervctl vm resume web-01

# Checkpoints
hypervctl checkpoint list web-01
hypervctl checkpoint create web-01 --name before-upgrade
hypervctl checkpoint restore web-01 before-upgrade
hypervctl checkpoint remove web-01 before-upgrade

# Switches and virtual hard disks
hypervctl switch list
hypervctl switch get 'Default Switch'
hypervctl vhd list --vm web-01
hypervctl vhd get 'C:\HyperV\web-01\disk.vhdx'

# The full-screen terminal interface
hypervctl tui
```

To try the tool without a Hyper-V host, or to develop against demo data, set
`HYPERVCTL_BACKEND=mock` (PowerShell):

```powershell
$env:HYPERVCTL_BACKEND = "mock"
hypervctl vm list
hypervctl tui
```

### Global options

| Option | Meaning |
|---|---|
| `-o, --output <table\|json\|csv>` | Output format (default `table`) |
| `--no-header` | Drop the header row of table and CSV output |
| `--no-trunc` | Print identifiers and names in full |
| `--color <auto\|always\|never>` | Colour policy (default `auto`) |
| `-y, --yes` | Skip confirmation prompts |
| `-v, --verbose` | Debug logs on stderr; `-vv` for trace |

### Waiting and timeouts

Power operations are asynchronous jobs in Hyper-V. Commands wait for
completion by default and show progress on stderr.

| Option | Meaning |
|---|---|
| `--no-wait` | Return once the backend has accepted the request |
| `--timeout <sec>` | Give up waiting after this many seconds (default 60; `0` waits forever) |

A timeout never cancels the job Hyper-V is already running; it only stops
*waiting* for it.

### Confirmation

`vm stop --force`, `vm restart --force`, `checkpoint restore` and
`checkpoint remove` ask for confirmation before running. Pass `-y` to skip
the prompt. When stdin is not a terminal, these commands refuse to run
without `-y` (exit code 2) instead of prompting.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | An operation failed (including partial failure across several targets) |
| 2 | Wrong usage |
| 3 | Target not found, or a name matched more than one target |
| 4 | Not permitted |
| 5 | Timed out |

A VM name that matches several machines is never guessed: the candidates and
their GUIDs are listed and the command exits with code 3. Targets can always
be given as a full GUID instead.

## Required permissions

Hyper-V administration rights are needed for the real backend: either run
from an elevated (Administrator) prompt, or add your account to the local
**Hyper-V Administrators** group. Without those rights commands fail with a
`PermissionDenied` error and exit code 4.

The machine must also have the Hyper-V feature enabled; if the
`root\virtualization\v2` WMI namespace is missing or the Virtual Machine
Management service (`vmms`) is stopped, commands fail with
`HyperVUnavailable`.

## TUI

`hypervctl tui` opens a two-pane dashboard: the VM list on top, the details
of the selected VM below (Details and Checkpoints tabs). It needs an
interactive terminal of at least 80x24 and is refused on a pipe.

Keys (full list under `?` inside the TUI): `j`/`k` or arrow keys move,
`Tab` switches focus between the panes, `g`/`G` jump to the first/last VM,
`Space` opens the action menu, `Enter` runs the selected menu entry,
`Esc` cancels or goes back, `1`/`2` pick the detail tab, `r` refreshes,
`q` or `Ctrl+C` quits.

## Limitations

- Windows only for the real backend. On other platforms the mock backend is
  used automatically so the tool can be tried anywhere.
- Local hosts only: remote Hyper-V connections are out of scope.
- MVP scope: VMs (list, detail, power, checkpoints), virtual switches
  (read-only), attached VHDs (read-only). Creating or deleting VMs, NIC/disk
  attachment, live migration, replication and GPU support are not included.
- Switch type (External / Internal / Private) is shown as `Unknown` because
  Hyper-V does not expose it through a single WMI property; determining it
  requires walking the port/uplink associations, which is not implemented yet.
- NIC IP addresses are not collected: they require guest integration services
  and are not available through WMI.
- Integration services are not collected yet on the real backend: the WMI
  component classes and their host-dependent availability still need
  verification, so the field is null (JSON) or `-` (table), which is distinct
  from an empty list.
- `vm restart` waits for a graceful guest shutdown followed by a start. If
  the guest OS does not respond to the shutdown request (e.g. integration
  services are not running), the command times out; use `vm restart --force`
  instead.
- The `shell` interactive mode from the original requirements is not
  implemented.

## Development

```console
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same checks on Linux and a build plus tests on Windows. Tests
never touch a real Hyper-V host; the WMI backend's hardware-in-the-loop tests
are excluded from CI and can be run manually on a Hyper-V host:

```console
cargo test -p hypervctl-backend-wmi --test real_machine -- --ignored
```

The hardware tests only mutate virtual machines whose names start with
`hypervctl-test-`. They are skipped if no such VM exists.

Golden output files can be regenerated with
`HYPERVCTL_BLESS=1 cargo test -p hypervctl golden` (and the equivalent for the
TUI snapshots).

## License

[MIT](LICENSE)
