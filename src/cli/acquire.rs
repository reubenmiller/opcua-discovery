//! `acquire` subcommand: run config-driven data acquisition (scan, poll,
//! subscribe, history) against an OPC UA server.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use clap::Args;
use tracing::info;

use crate::acquire::config::{AcquireConfig, ConnectionConfig, TaskConfig};
use crate::acquire::{self};
use crate::cli::parse_duration;
use crate::error::{Result, ScanError};

/// Arguments for the `acquire` command.
#[derive(Debug, Args)]
pub struct AcquireArgs {
    /// Acquisition config file (TOML). With --tedge-dot-config it may only hold
    /// [acquire], [output] and [[task]].
    #[arg(long, value_name = "FILE", conflicts_with = "config_dir")]
    pub config: Option<PathBuf>,

    /// Directory of acquisition config files (`*.toml`), run in order.
    #[arg(long = "config-dir", value_name = "DIR")]
    pub config_dir: Option<PathBuf>,

    /// Acquire the points a tedge-dot OPC UA connector config defines for a
    /// device (inline points and points_from libraries), with its connection.
    #[arg(long, value_name = "FILE", conflicts_with = "config_dir")]
    pub tedge_dot_config: Option<PathBuf>,

    /// Device in the tedge-dot config; optional when it has one device.
    #[arg(long, value_name = "NAME", requires = "tedge_dot_config")]
    pub device: Option<String>,

    /// Bound continuous tasks (poll/subscribe) to this duration, e.g. 30s, 5m.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub duration: Option<Duration>,

    /// Force all nodes to be polled, disabling subscriptions regardless of any
    /// per-node or default subscribe preference in the config.
    #[arg(long)]
    pub poll_only: bool,
}

/// Run the `acquire` command.
pub async fn run(args: AcquireArgs, cancel: Arc<AtomicBool>) -> Result<()> {
    if let Some(tedge_dot) = &args.tedge_dot_config {
        return run_tedge_dot(&args, tedge_dot, cancel).await;
    }

    let configs: Vec<(PathBuf, AcquireConfig)> = match (&args.config, &args.config_dir) {
        (Some(path), _) => vec![(path.clone(), AcquireConfig::from_file(path)?)],
        (None, Some(dir)) => acquire::load_config_dir(dir)?,
        (None, None) => {
            return Err(ScanError::InvalidArgument(
                "one of --config, --config-dir or --tedge-dot-config is required".into(),
            ));
        }
    };

    for (path, mut config) in configs {
        info!(config = %path.display(), "starting acquisition");
        // CLI override wins: once set it can only force poll-only on, never off.
        config.acquire.poll_only |= args.poll_only;
        let connection = config.connection_args()?;
        acquire::run(
            config,
            connection,
            args.duration,
            Some(path.as_path()),
            cancel.clone(),
        )
        .await?;
    }

    Ok(())
}

/// Acquire a tedge-dot device's points: connection and selection from the
/// connector config, tasks and output from the optional extras file (else
/// tedge-dot's intervals). Nothing is written back to either file.
async fn run_tedge_dot(
    args: &AcquireArgs,
    tedge_dot: &std::path::Path,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let points = crate::tedge_dot::points::load(tedge_dot, args.device.as_deref())?;

    let mut connection = ConnectionConfig::default().to_args()?;
    connection.tedge_dot_config = Some(tedge_dot.to_path_buf());
    connection.device = args.device.clone();
    crate::tedge_dot::connection::apply(&mut connection)?;

    let mut config = match &args.config {
        Some(extras) => AcquireConfig::extras_from_file(extras)?,
        None => AcquireConfig::default(),
    };
    if config.tasks.is_empty() {
        config.tasks = vec![
            TaskConfig::Poll {
                interval: points.poll_interval,
            },
            TaskConfig::Subscribe {
                publishing_interval: None,
                sampling_interval: Some(points.sampling_interval.unwrap_or(points.poll_interval)),
                queue_size: None,
            },
        ];
    }
    config.selection.nodes = points.selectors;
    config.acquire.poll_only |= args.poll_only;

    info!(
        config = %tedge_dot.display(),
        points = config.selection.nodes.len(),
        skipped = points.skipped.len(),
        "starting acquisition from tedge-dot config"
    );
    acquire::run(config, connection, args.duration, None, cancel).await
}
