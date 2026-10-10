//! Command line definition (design 4.3).

use clap::{Args, Parser, Subcommand, ValueEnum};

/// The whole command line (spec 2.1).
#[derive(Debug, Parser)]
#[command(name = "hypervctl", version, about = "Hyper-V from the command line")]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub resource: Resource,
}

/// Options every command takes, wherever they are written on the line
/// (spec 2.2).
#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Output format
    #[arg(short = 'o', long, value_enum, global = true, default_value = "table")]
    pub output: Format,

    /// Skip the header row of table and CSV output
    #[arg(long, global = true)]
    pub no_header: bool,

    /// Print identifiers in full and never cut a name short
    #[arg(long, global = true)]
    pub no_trunc: bool,

    /// When to colour the output
    #[arg(long, value_enum, global = true, default_value = "auto")]
    pub color: ColorChoice,

    /// Skip confirmation prompts
    #[arg(short = 'y', long, global = true)]
    #[allow(dead_code)] // Read by the confirmation prompt, which arrives with M5.
    pub yes: bool,

    /// Detail on stderr: -v for debug, -vv for trace
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// Output formats (spec 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
    Csv,
}

/// Colour policy (spec 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

/// The resource a command works on (spec 2.1).
#[derive(Debug, Subcommand)]
pub enum Resource {
    /// Virtual machines
    Vm {
        #[command(subcommand)]
        command: VmCommand,
    },
    /// Checkpoints
    Checkpoint {
        #[command(subcommand)]
        command: CheckpointCommand,
    },
    /// Virtual switches
    Switch {
        #[command(subcommand)]
        command: SwitchCommand,
    },
    /// Virtual hard disks
    Vhd {
        #[command(subcommand)]
        command: VhdCommand,
    },
}

/// Read-only verbs of `vm` (spec 2.3).
#[derive(Debug, Subcommand)]
pub enum VmCommand {
    /// List the virtual machines
    #[command(visible_alias = "ls")]
    List,

    /// Show one virtual machine in full
    Get {
        /// Name or GUID of the VM
        vm: String,
    },
}

/// Read-only verbs of `checkpoint` (spec 2.4).
#[derive(Debug, Subcommand)]
pub enum CheckpointCommand {
    /// List the checkpoints of one virtual machine
    #[command(visible_alias = "ls")]
    List {
        /// Name or GUID of the VM
        vm: String,
    },
}

/// Read-only verbs of `switch` (spec 2.5).
#[derive(Debug, Subcommand)]
pub enum SwitchCommand {
    /// List the virtual switches
    #[command(visible_alias = "ls")]
    List,

    /// Show one virtual switch in full
    Get {
        /// Name or GUID of the switch
        switch: String,
    },
}

/// Read-only verbs of `vhd` (spec 2.5).
#[derive(Debug, Subcommand)]
pub enum VhdCommand {
    /// List the virtual disks attached to a virtual machine
    #[command(visible_alias = "ls")]
    List {
        /// Only the disks of this VM (name or GUID)
        #[arg(long)]
        vm: Option<String>,
    },

    /// Show the file information of one virtual disk
    Get {
        /// Path of the virtual disk, passed to the backend unchanged
        path: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    fn ok_parse(args: &[&str]) -> Cli {
        match parse(args) {
            Ok(cli) => cli,
            Err(error) => panic!("expected {:?} to parse, got {error}", args),
        }
    }

    fn output_of(args: &[&str]) -> Format {
        ok_parse(args).global.output
    }

    #[test]
    fn the_read_only_verbs_of_every_resource_parse() {
        assert!(matches!(
            ok_parse(&["hypervctl", "vm", "list"]).resource,
            Resource::Vm {
                command: VmCommand::List
            }
        ));
        assert!(matches!(
            ok_parse(&["hypervctl", "checkpoint", "list", "web-01"]).resource,
            Resource::Checkpoint {
                command: CheckpointCommand::List { vm }
            } if vm == "web-01"
        ));
        assert!(matches!(
            ok_parse(&["hypervctl", "switch", "get", "test-external"]).resource,
            Resource::Switch {
                command: SwitchCommand::Get { switch }
            } if switch == "test-external"
        ));
        assert!(matches!(
            ok_parse(&["hypervctl", "vhd", "get", r"C:\vhds\disk.vhdx"]).resource,
            Resource::Vhd {
                command: VhdCommand::Get { path }
            } if path == r"C:\vhds\disk.vhdx"
        ));
    }

    #[test]
    fn list_is_also_spelled_ls() {
        assert!(matches!(
            ok_parse(&["hypervctl", "vm", "ls"]).resource,
            Resource::Vm {
                command: VmCommand::List
            }
        ));
        assert!(matches!(
            ok_parse(&["hypervctl", "vhd", "ls"]).resource,
            Resource::Vhd {
                command: VhdCommand::List { vm: None }
            }
        ));
    }

    #[test]
    fn vhd_list_takes_an_optional_vm_filter() {
        let filtered = ok_parse(&["hypervctl", "vhd", "list", "--vm", "web-01"]);
        assert!(matches!(
            filtered.resource,
            Resource::Vhd {
                command: VhdCommand::List {
                    vm: Some(name)
                }
            } if name == "web-01"
        ));
        let all = ok_parse(&["hypervctl", "vhd", "list"]);
        assert!(matches!(
            all.resource,
            Resource::Vhd {
                command: VhdCommand::List { vm: None }
            }
        ));
    }

    #[test]
    fn global_options_work_before_and_after_the_subcommand() {
        assert_eq!(
            output_of(&["hypervctl", "-o", "json", "vm", "list"]),
            Format::Json
        );
        assert_eq!(
            output_of(&["hypervctl", "vm", "list", "-o", "csv"]),
            Format::Csv
        );
        assert_eq!(
            output_of(&["hypervctl", "switch", "get", "test-external", "-o", "json"]),
            Format::Json
        );
        let global = ok_parse(&[
            "hypervctl",
            "vm",
            "get",
            "web-01",
            "--no-header",
            "--no-trunc",
        ]);
        assert!(global.global.no_header);
        assert!(global.global.no_trunc);
    }

    #[test]
    fn the_defaults_match_the_specification() {
        let cli = ok_parse(&["hypervctl", "vm", "list"]);
        assert_eq!(cli.global.output, Format::Table);
        assert_eq!(cli.global.color, ColorChoice::Auto);
        assert!(!cli.global.no_header);
        assert!(!cli.global.no_trunc);
        assert!(!cli.global.yes);
        assert_eq!(cli.global.verbose, 0);
    }

    #[test]
    fn verbose_can_be_repeated() {
        let cli = ok_parse(&["hypervctl", "-vv", "vm", "list"]);
        assert_eq!(cli.global.verbose, 2);
        let one = ok_parse(&["hypervctl", "vm", "list", "-v"]);
        assert_eq!(one.global.verbose, 1);
    }

    #[test]
    fn an_unknown_resource_or_verb_is_a_usage_error() {
        assert!(parse(&["hypervctl", "snapshot", "list"]).is_err());
        assert!(parse(&["hypervctl", "vm", "start", "web-01"]).is_err());
        assert!(parse(&["hypervctl", "vm"]).is_err());
        assert!(parse(&["hypervctl", "--output", "yaml", "vm", "list"]).is_err());
    }

    #[test]
    fn a_missing_target_is_a_usage_error() {
        assert!(parse(&["hypervctl", "vm", "get"]).is_err());
        assert!(parse(&["hypervctl", "checkpoint", "list"]).is_err());
    }
}
