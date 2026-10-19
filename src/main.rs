//! `opcua-scan` — a production-grade CLI for discovering an OPC UA namespace.

mod acquire;
mod cache;
mod cli;
mod config;
mod error;
mod export;
mod filters;
mod logging;
mod opcua;
mod scanner;
mod signal;
mod subscription;
mod tedge_dot;
mod tui;

use clap::Parser;

use cli::{Cli, Command};
use config::Profile;
use error::Result;

#[tokio::main]
async fn main() {
    let mut cli = Cli::parse();
    logging::init(cli.verbose, cli.quiet);

    if let Err(err) = apply_profile(&mut cli).and_then(|()| apply_tedge_dot(&mut cli)) {
        eprintln!("error: {err}");
        std::process::exit(err.exit_code());
    }

    let cancel = signal::install();
    let cancel_check = cancel.clone();

    let result = match cli.command {
        Command::Connect(args) => cli::connect::run(args).await,
        Command::Browse(args) => cli::browse::run(args, cancel).await,
        Command::Search(args) => cli::search::run(args, cancel).await,
        Command::Sample(args) => cli::sample::run(args, cancel).await,
        Command::Acquire(args) => cli::acquire::run(args, cancel).await,
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(err.exit_code());
    }

    // A clean return after a Ctrl+C still reports cancellation via exit code 4.
    if cancel_check.load(std::sync::atomic::Ordering::Relaxed) {
        std::process::exit(4);
    }
}

/// Fill the connection from `--tedge-dot-config` (if any). Runs after the
/// profile, so explicit CLI flags win over both.
fn apply_tedge_dot(cli: &mut Cli) -> Result<()> {
    let connection = match &mut cli.command {
        Command::Connect(args) => &mut args.connection,
        Command::Browse(args) => &mut args.connection,
        Command::Search(args) => &mut args.connection,
        Command::Sample(args) => &mut args.connection,
        Command::Acquire(_) => return Ok(()),
    };
    tedge_dot::connection::apply(connection)
}

/// Load the `--profile` (if any) and merge its defaults into the parsed args.
fn apply_profile(cli: &mut Cli) -> Result<()> {
    let Some(name) = cli.profile.clone() else {
        return Ok(());
    };
    let profile = Profile::load(&name)?;
    match &mut cli.command {
        Command::Connect(args) => profile.apply_connection(&mut args.connection),
        Command::Browse(args) => {
            profile.apply(&mut args.connection, &mut args.traversal, &mut args.filters)
        }
        Command::Search(args) => {
            profile.apply(&mut args.connection, &mut args.traversal, &mut args.filters)
        }
        Command::Sample(args) => {
            profile.apply(&mut args.connection, &mut args.traversal, &mut args.filters)
        }
        // The acquire command is fully config-file driven; profiles do not apply.
        Command::Acquire(_) => Ok(()),
    }
}
