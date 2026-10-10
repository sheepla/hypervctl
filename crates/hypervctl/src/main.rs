//! `hypervctl` binary: the command line frontend (design 4.1 and 4.2).

mod cli;
mod tui;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::filter::LevelFilter;

use hypervctl_backend_mock::MockBackend;
use hypervctl_core::{BackendActor, HyperVError, Operations, RawError};

use crate::cli::args::Cli;
use crate::cli::commands::Context;
use crate::cli::errors::{self, CliError};
use crate::cli::output;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let opts = output::options(&cli.global);
    init_tracing(cli.global.verbose);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let error = CliError::Backend(HyperVError::backend(RawError::new(format!(
                "could not start the runtime: {error}"
            ))));
            errors::print(&error, cli.global.verbose);
            return ExitCode::from(errors::exit_code(&error));
        }
    };

    let context = Context::new(&cli.global, &opts);
    let outcome = runtime.block_on(async {
        match ensure_backend()? {
            BackendChoice::Mock => {
                let ops = Operations::new(BackendActor::spawn(MockBackend::with_fixture));
                // Ctrl+C stops the waiting, not the work: what the backend has
                // accepted keeps running there (design 4.2).
                tokio::select! {
                    result = cli::commands::run(&cli.resource, &ops, &context) => result,
                    interrupted = tokio::signal::ctrl_c() => {
                        let _ = interrupted;
                        Err(CliError::Interrupted)
                    }
                }
            }
        }
    });

    match outcome {
        Ok(output) => {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(output.stdout.as_bytes());
            let mut stderr = std::io::stderr().lock();
            let _ = stderr.write_all(output.stderr.as_bytes());
            ExitCode::from(output.code)
        }
        Err(error) => {
            errors::print(&error, cli.global.verbose);
            ExitCode::from(errors::exit_code(&error))
        }
    }
}

/// Which backend to start. The WMI backend is not built yet, so Windows asks
/// for the mock explicitly instead of failing halfway through a command
/// (design 1.2).
enum BackendChoice {
    Mock,
}

fn ensure_backend() -> Result<BackendChoice, CliError> {
    let value = std::env::var_os("HYPERVCTL_BACKEND")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    match value.as_str() {
        "mock" => Ok(BackendChoice::Mock),
        // No value and `wmi` mean the real backend: only non-Windows may fall
        // back to the mock, and even there the difference is reported.
        "" | "wmi" => {
            if cfg!(windows) {
                Err(CliError::Backend(HyperVError::backend(RawError::new(
                    "the WMI backend is not available yet; set HYPERVCTL_BACKEND=mock",
                ))))
            } else {
                Ok(BackendChoice::Mock)
            }
        }
        other => Err(CliError::Backend(HyperVError::backend(RawError::new(
            format!("unknown HYPERVCTL_BACKEND value: {other}"),
        )))),
    }
}

/// `-v` writes debug logs, `-vv` trace logs, both to stderr (design 4.1).
fn init_tracing(verbose: u8) {
    if verbose == 0 {
        return;
    }
    let level = if verbose > 1 {
        LevelFilter::TRACE
    } else {
        LevelFilter::DEBUG
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .try_init();
}
