//! `sample` subcommand: subscribe to matching variables and print live updates.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use clap::Args;
use opcua::types::NodeId;
use tokio::sync::mpsc;

use crate::cli::{ConnectionArgs, FilterArgs, TraversalArgs, parse_duration};
use crate::error::Result;
use crate::export::NodeRecord;
use crate::filters::FilterSet;
use crate::opcua::Connection;
use crate::scanner::{Engine, ScanConfig, resolve_root};
use crate::subscription;

/// Arguments for the `sample` command.
#[derive(Debug, Args)]
pub struct SampleArgs {
    /// Connection / authentication options.
    #[command(flatten)]
    pub connection: ConnectionArgs,
    /// Traversal control (used to locate variables to sample).
    #[command(flatten)]
    pub traversal: TraversalArgs,
    /// Node filters (which variables to sample).
    #[command(flatten)]
    pub filters: FilterArgs,

    /// How long to sample for, e.g. 10s, 1m.
    #[arg(long, value_name = "DURATION", default_value = "10s", value_parser = parse_duration)]
    pub duration: Duration,
}

/// Run the `sample` command.
pub async fn run(mut args: SampleArgs, cancel: Arc<AtomicBool>) -> Result<()> {
    // Sampling only makes sense for subscribable variables.
    args.filters.subscribable = true;

    let conn = Connection::connect(&args.connection).await?;
    let session = conn.session().clone();

    let namespaces =
        Arc::new(crate::cli::load_namespaces(&session, &args.connection, conn.endpoint()).await?);
    let filters = Arc::new(FilterSet::compile(&args.filters, &namespaces)?);
    let root = resolve_root(&args.traversal.root)?;

    let config = Arc::new(ScanConfig {
        root,
        depth: args.traversal.depth,
        strategy: args.traversal.strategy,
        workers: args.traversal.workers,
        page_size: args.traversal.page_size,
        max_children: args.traversal.max_children,
        retry: args.connection.retry,
        preview_values: false,
    });

    let engine = Engine::new(
        session.clone(),
        config,
        filters,
        namespaces.clone(),
        cancel.clone(),
    );
    let (tx, mut rx) = mpsc::channel::<NodeRecord>(1024);
    let engine_task = tokio::spawn(async move { engine.run(tx).await });

    let mut nodes: Vec<(String, NodeId)> = Vec::new();
    while let Some(record) = rx.recv().await {
        if let Ok(node_id) = NodeId::from_str(&record.node_id) {
            nodes.push((record.browse_path, node_id));
        }
    }
    let _ = engine_task.await;

    eprintln!(
        "sampling {} variable(s) for {:?} (Ctrl+C to stop)...",
        nodes.len(),
        args.duration
    );
    subscription::run_sample(&session, nodes, args.duration, cancel).await;

    conn.disconnect().await;
    Ok(())
}
