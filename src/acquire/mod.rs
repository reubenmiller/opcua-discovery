//! Acquisition runner: connects once and drives all configured tasks.

pub mod config;
pub mod generate;
pub mod selection;
pub mod sink;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use opcua::client::Session;
use opcua::types::NodeId;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::error::{Result, ScanError};
use crate::export::NodeRecord;
use crate::filters::FilterSet;
use crate::opcua::Connection;
use crate::opcua::history::{self, HistoryQuery};
use crate::opcua::namespaces::NamespaceTable;
use crate::opcua::value::read_readings;
use crate::scanner::{Engine, ScanConfig, resolve_root};
use crate::subscription::{self, SubscriptionParams};

use config::{AcquireConfig, NodeMode, TaskConfig};
use selection::SelectedNode;
use sink::{Sample, SampleMeta, Sink};

/// Run a single acquisition config to completion.
///
/// `duration` optionally bounds continuous tasks (poll/subscribe); one-shot
/// tasks (scan/history) always run to completion regardless. `config_path`, when
/// given, is the file the config was loaded from; reconstructed browse paths are
/// cached back into it.
pub async fn run(
    config: AcquireConfig,
    connection_args: crate::cli::ConnectionArgs,
    duration: Option<Duration>,
    config_path: Option<&std::path::Path>,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    // `cancel` is the process-wide Ctrl+C flag, which `main` reports as exit
    // code 4. The duration must end the run without looking like an
    // interruption, so the tasks watch a run-local flag that follows Ctrl+C
    // and is also set by the duration timer.
    let signal = cancel;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let (signal, cancel) = (signal.clone(), cancel.clone());
        tokio::spawn(async move {
            while !cancel.load(Ordering::Relaxed) {
                if signal.load(Ordering::Relaxed) {
                    cancel.store(true, Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }
    let result = run_tasks(
        config,
        connection_args,
        duration,
        config_path,
        cancel.clone(),
    )
    .await;
    // Ends the watcher, so no watcher outlives its run (--config-dir runs several).
    cancel.store(true, Ordering::Relaxed);
    result
}

/// The body of [`run`], with the run-local stop flag as `cancel`.
async fn run_tasks(
    config: AcquireConfig,
    connection_args: crate::cli::ConnectionArgs,
    duration: Option<Duration>,
    config_path: Option<&std::path::Path>,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let conn = Connection::connect(&connection_args).await?;
    let session = conn.session().clone();

    let namespaces =
        Arc::new(crate::cli::load_namespaces(&session, &connection_args, conn.endpoint()).await?);
    let sink = Arc::new(Sink::new(&config.output)?);

    // Resolve concrete nodes once; shared by poll/subscribe/history tasks.
    let needs_nodes = config
        .tasks
        .iter()
        .any(|t| !matches!(t, TaskConfig::Scan { .. }));
    let nodes = if needs_nodes {
        let resolved = selection::resolve(
            &session,
            &config,
            &namespaces,
            conn.endpoint(),
            cancel.clone(),
        )
        .await?;
        // Cache any reconstructed browse paths back into the source config so
        // later runs can skip the inverse browse.
        if let Some(path) = config_path
            && !resolved.path_updates.is_empty()
        {
            if let Err(e) = persist_node_paths(path, &resolved.path_updates) {
                warn!(error = %e, "failed to cache browse paths into config");
            } else {
                info!(
                    count = resolved.path_updates.len(),
                    config = %path.display(),
                    "cached reconstructed browse paths into config"
                );
            }
        }
        resolved.nodes
    } else {
        Vec::new()
    };

    if needs_nodes && nodes.is_empty() {
        warn!("no nodes matched the selection; poll/subscribe/history tasks will be idle");
    } else if needs_nodes {
        info!(count = nodes.len(), "selected nodes for acquisition");
        for node in &nodes {
            tracing::debug!(name = %node.name, path = %node.path, node_id = %node.node_id, "selected");
        }
    }

    let has_continuous = config
        .tasks
        .iter()
        .any(|t| matches!(t, TaskConfig::Poll { .. } | TaskConfig::Subscribe { .. }));

    let mut set: JoinSet<()> = JoinSet::new();

    // Scan and history tasks each operate on the full selection independently.
    // Poll/subscribe tasks are not spawned per-block; instead the selection is
    // partitioned once by each node's effective mode (see below), so a single
    // run can poll some nodes while subscribing to others.
    for task in config.tasks.clone() {
        match task {
            TaskConfig::Scan { .. } | TaskConfig::History { .. } => {
                spawn_task(
                    &mut set,
                    task,
                    session.clone(),
                    sink.clone(),
                    nodes.clone(),
                    namespaces.clone(),
                    config.clone(),
                    cancel.clone(),
                );
            }
            TaskConfig::Poll { .. } | TaskConfig::Subscribe { .. } => {}
        }
    }

    // Route the selected nodes to polling or subscribing based on their
    // per-node mode, the effective default, and the global poll-only override.
    if has_continuous && !nodes.is_empty() {
        let default_mode = config.effective_default_mode();
        let poll_only = config.acquire.poll_only;
        let (poll_nodes, subscribe_nodes) = partition_nodes(&nodes, default_mode, poll_only);
        info!(
            poll = poll_nodes.len(),
            subscribe = subscribe_nodes.len(),
            poll_only,
            "routed nodes by acquisition mode"
        );

        if !poll_nodes.is_empty() {
            let interval = config.poll_interval();
            let session = session.clone();
            let sink = sink.clone();
            let cancel = cancel.clone();
            set.spawn(async move {
                run_poll(&session, &poll_nodes, &sink, interval, cancel).await;
            });
        }

        if !subscribe_nodes.is_empty() {
            let sp = config.subscribe_params();
            let defaults = SubscriptionParams::default();
            let params = SubscriptionParams {
                publishing_interval: sp
                    .publishing_interval
                    .unwrap_or(defaults.publishing_interval),
                sampling_interval: sp.sampling_interval.unwrap_or(defaults.sampling_interval),
                queue_size: sp.queue_size.unwrap_or(defaults.queue_size),
            };
            let session = session.clone();
            let sink = sink.clone();
            let cancel = cancel.clone();
            set.spawn(async move {
                subscription::run_subscription(&session, subscribe_nodes, params, sink, cancel)
                    .await;
            });
        }
    }

    // Bound continuous tasks by duration if requested.
    if has_continuous && let Some(d) = duration {
        let timer_cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(d).await;
            timer_cancel.store(true, Ordering::Relaxed);
        });
    }

    while set.join_next().await.is_some() {}

    sink.finish()?;
    conn.disconnect().await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_task(
    set: &mut JoinSet<()>,
    task: TaskConfig,
    session: Arc<Session>,
    sink: Arc<Sink>,
    nodes: Vec<SelectedNode>,
    namespaces: Arc<NamespaceTable>,
    config: AcquireConfig,
    cancel: Arc<AtomicBool>,
) {
    match task {
        TaskConfig::Scan {
            strategy,
            workers,
            page_size,
            max_children,
            preview_values,
        } => {
            set.spawn(async move {
                let result = run_scan(
                    &session,
                    &config,
                    &namespaces,
                    &sink,
                    ScanTaskParams {
                        strategy: strategy.into(),
                        workers: workers.unwrap_or(8),
                        page_size: page_size.unwrap_or(200),
                        max_children,
                        preview_values,
                    },
                    cancel,
                )
                .await;
                if let Err(e) = result {
                    warn!(error = %e, "scan task failed");
                }
            });
        }
        // Poll/subscribe are not spawned per-task: nodes are partitioned by
        // their effective mode in `run` and routed there instead.
        TaskConfig::Poll { .. } | TaskConfig::Subscribe { .. } => {}
        TaskConfig::History {
            start,
            end,
            max_values,
        } => {
            set.spawn(async move {
                let result = run_history(&session, &nodes, &sink, &start, end, max_values).await;
                if let Err(e) = result {
                    warn!(error = %e, "history task failed");
                }
            });
        }
    }
}

/// Resolve a node's effective acquisition mode and split the selection into
/// `(poll_nodes, subscribe_nodes)`.
///
/// `default_mode` is applied to nodes left at [`NodeMode::Auto`]; `poll_only`
/// forces every node to poll regardless of its configured mode.
fn partition_nodes(
    nodes: &[SelectedNode],
    default_mode: NodeMode,
    poll_only: bool,
) -> (Vec<SelectedNode>, Vec<SelectedNode>) {
    let mut poll_nodes = Vec::new();
    let mut subscribe_nodes = Vec::new();
    for node in nodes {
        if effective_subscribe(node, default_mode, poll_only) {
            subscribe_nodes.push(node.clone());
        } else {
            poll_nodes.push(node.clone());
        }
    }
    (poll_nodes, subscribe_nodes)
}

/// Whether a node should be subscribed (`true`) or polled (`false`) given the
/// effective default and the global poll-only override.
fn effective_subscribe(node: &SelectedNode, default_mode: NodeMode, poll_only: bool) -> bool {
    if poll_only {
        return false;
    }
    let mode = match node.mode {
        NodeMode::Auto => default_mode,
        explicit => explicit,
    };
    match mode {
        NodeMode::Subscribe => true,
        NodeMode::Poll => false,
        // Default is itself auto: prefer subscribe only when the server reports
        // the node as monitorable, otherwise fall back to polling.
        NodeMode::Auto => node.subscribable,
    }
}

struct ScanTaskParams {
    strategy: crate::cli::Strategy,
    workers: usize,
    page_size: u32,
    max_children: Option<usize>,
    preview_values: bool,
}
async fn run_scan(
    session: &Arc<Session>,
    config: &AcquireConfig,
    namespaces: &Arc<NamespaceTable>,
    sink: &Arc<Sink>,
    params: ScanTaskParams,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let filter_args = config.filter_args()?;
    let filters = Arc::new(FilterSet::compile(&filter_args, namespaces)?);
    let root = resolve_root(&config.selection.root)?;

    let scan_config = Arc::new(ScanConfig {
        root,
        depth: config.selection.depth()?,
        strategy: params.strategy,
        workers: params.workers,
        page_size: params.page_size,
        max_children: params.max_children,
        retry: config.connection.retry.unwrap_or(3),
        preview_values: params.preview_values,
    });

    let engine = Engine::new(
        session.clone(),
        scan_config,
        filters,
        namespaces.clone(),
        cancel,
    );
    let (tx, mut rx) = mpsc::channel::<NodeRecord>(1024);
    let engine_task = tokio::spawn(async move { engine.run(tx).await });

    while let Some(record) = rx.recv().await {
        sink.emit_record(&record)?;
    }
    let _ = engine_task.await;
    Ok(())
}

async fn run_poll(
    session: &Arc<Session>,
    nodes: &[SelectedNode],
    sink: &Arc<Sink>,
    interval: Duration,
    cancel: Arc<AtomicBool>,
) {
    if nodes.is_empty() {
        return;
    }
    let node_ids: Vec<NodeId> = nodes.iter().map(|n| n.node_id.clone()).collect();
    let metas: Vec<Arc<SampleMeta>> = nodes.iter().map(|n| Arc::new(n.meta.clone())).collect();

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        ticker.tick().await;
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        match read_readings(session, &node_ids).await {
            Ok(readings) => {
                for ((node, reading), meta) in nodes.iter().zip(readings.iter()).zip(metas.iter()) {
                    let sample = Sample {
                        name: node.name.clone(),
                        node_id: node.node_id.to_string(),
                        value: reading.value.clone(),
                        time: reading.best_time(),
                        status: reading.status.clone(),
                        meta: meta.clone(),
                    };
                    if let Err(e) = sink.emit_sample(&sample) {
                        warn!(error = %e, "failed to emit poll sample");
                    }
                }
            }
            Err(e) => warn!(error = %e, "poll read failed"),
        }
    }
}

async fn run_history(
    session: &Arc<Session>,
    nodes: &[SelectedNode],
    sink: &Arc<Sink>,
    start: &str,
    end: Option<String>,
    max_values: Option<u32>,
) -> Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    let query = HistoryQuery {
        start: history::parse_time(start)?,
        end: match end {
            Some(e) => history::parse_time(&e)?,
            None => opcua::types::DateTime::now(),
        },
        max_values: max_values.unwrap_or(0),
    };

    for node in nodes {
        let meta = Arc::new(node.meta.clone());
        match history::read_raw(session, &node.node_id, &query).await {
            Ok(readings) => {
                for reading in &readings {
                    let sample = Sample {
                        name: node.name.clone(),
                        node_id: node.node_id.to_string(),
                        value: reading.value.clone(),
                        time: reading.best_time(),
                        status: reading.status.clone(),
                        meta: meta.clone(),
                    };
                    sink.emit_sample(&sample)?;
                }
            }
            Err(e) => warn!(node = %node.node_id, error = %e, "history read failed"),
        }
    }
    Ok(())
}

/// Write reconstructed browse paths back into the `[[selection.node]]` entries
/// of an existing config file, preserving its formatting and comments.
fn persist_node_paths(path: &std::path::Path, updates: &[(String, String)]) -> Result<()> {
    let original = std::fs::read_to_string(path).map_err(|e| ScanError::Config {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let updated = generate::update_node_paths_in_toml(&original, updates)?;
    if updated != original {
        std::fs::write(path, updated).map_err(|e| ScanError::Config {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
    }
    Ok(())
}

/// Load every `*.toml` config in a directory (sorted), erroring if none exist.
pub fn load_config_dir(dir: &std::path::Path) -> Result<Vec<(std::path::PathBuf, AcquireConfig)>> {
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| ScanError::Config {
            path: dir.to_path_buf(),
            reason: e.to_string(),
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
        .collect();
    entries.sort();

    if entries.is_empty() {
        return Err(ScanError::Config {
            path: dir.to_path_buf(),
            reason: "no *.toml config files found in directory".to_string(),
        });
    }

    let mut configs = Vec::with_capacity(entries.len());
    for path in entries {
        let config = AcquireConfig::from_file(&path)?;
        configs.push((path, config));
    }
    Ok(configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acquire::sink::SampleMeta;

    fn node(mode: NodeMode, subscribable: bool) -> SelectedNode {
        SelectedNode {
            name: "n".to_string(),
            path: "n".to_string(),
            node_id: NodeId::new(2, 1),
            mode,
            subscribable,
            meta: SampleMeta::default(),
        }
    }

    #[test]
    fn poll_only_forces_polling() {
        assert!(!effective_subscribe(
            &node(NodeMode::Subscribe, true),
            NodeMode::Subscribe,
            true
        ));
    }

    #[test]
    fn explicit_mode_wins_over_default() {
        assert!(effective_subscribe(
            &node(NodeMode::Subscribe, false),
            NodeMode::Poll,
            false
        ));
        assert!(!effective_subscribe(
            &node(NodeMode::Poll, true),
            NodeMode::Subscribe,
            false
        ));
    }

    #[test]
    fn auto_follows_non_auto_default() {
        assert!(effective_subscribe(
            &node(NodeMode::Auto, false),
            NodeMode::Subscribe,
            false
        ));
        assert!(!effective_subscribe(
            &node(NodeMode::Auto, true),
            NodeMode::Poll,
            false
        ));
    }

    #[test]
    fn auto_default_uses_subscribability() {
        assert!(effective_subscribe(
            &node(NodeMode::Auto, true),
            NodeMode::Auto,
            false
        ));
        assert!(!effective_subscribe(
            &node(NodeMode::Auto, false),
            NodeMode::Auto,
            false
        ));
    }

    #[test]
    fn partition_splits_by_effective_mode() {
        let nodes = vec![
            node(NodeMode::Subscribe, false),
            node(NodeMode::Poll, true),
            node(NodeMode::Auto, true),
            node(NodeMode::Auto, false),
        ];
        let (poll, subscribe) = partition_nodes(&nodes, NodeMode::Auto, false);
        // subscribe-explicit + auto-subscribable
        assert_eq!(subscribe.len(), 2);
        // poll-explicit + auto-not-subscribable
        assert_eq!(poll.len(), 2);

        // poll_only collapses everything to polling.
        let (poll, subscribe) = partition_nodes(&nodes, NodeMode::Auto, true);
        assert_eq!(poll.len(), 4);
        assert!(subscribe.is_empty());
    }
}
