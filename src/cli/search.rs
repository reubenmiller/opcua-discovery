//! `search` subcommand: fuzzy-find nodes across the address space.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::Args;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use tokio::sync::mpsc;

use crate::cli::{ConnectionArgs, FilterArgs, OutputArgs, TraversalArgs};
use crate::error::Result;
use crate::export::{NodeRecord, render_display, write_export};
use crate::filters::FilterSet;
use crate::opcua::Connection;
use crate::scanner::{Engine, ScanConfig, resolve_root};

/// Arguments for the `search` command.
#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Connection / authentication options.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// The text to search for (fuzzy-matched against names, path and description).
    #[arg(value_name = "QUERY")]
    pub query: Option<String>,

    /// Traversal control.
    #[command(flatten)]
    pub traversal: TraversalArgs,
    /// Node filters (applied before searching).
    #[command(flatten)]
    pub filters: FilterArgs,
    /// Output / export options.
    #[command(flatten)]
    pub output: OutputArgs,

    /// Maximum number of results to return.
    #[arg(long, default_value_t = 50, value_name = "N")]
    pub limit: usize,
}

/// Run the `search` command.
pub async fn run(args: SearchArgs, cancel: Arc<AtomicBool>) -> Result<()> {
    let query = args.query.clone().ok_or_else(|| {
        crate::error::ScanError::InvalidArgument("a search QUERY is required".into())
    })?;

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
        preview_values: args.output.preview_values,
    });

    let engine = Engine::new(session.clone(), config, filters, namespaces.clone(), cancel);
    let (tx, mut rx) = mpsc::channel::<NodeRecord>(1024);
    let engine_task = tokio::spawn(async move { engine.run(tx).await });

    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, NodeRecord)> = Vec::new();
    while let Some(record) = rx.recv().await {
        if let Some(score) = score_record(&matcher, &query, &record) {
            scored.push((score, record));
        }
    }
    let _ = engine_task.await;

    // Highest score first.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.truncate(args.limit);
    let mut records: Vec<NodeRecord> = scored.into_iter().map(|(_, r)| r).collect();

    if let Some(encoding) = args.output.output {
        write_export(&mut records, encoding, args.output.file.as_deref())?;
    } else {
        render_display(&mut records, args.output.format)?;
    }

    eprintln!("\n{} match(es) for \"{}\"", records.len(), query);
    conn.disconnect().await;
    Ok(())
}

/// Score a record against the query, returning the best field score (if any).
fn score_record(matcher: &SkimMatcherV2, query: &str, record: &NodeRecord) -> Option<i64> {
    let fields = [
        record.browse_name.as_str(),
        record.display_name.as_str(),
        record.browse_path.as_str(),
        record.description.as_deref().unwrap_or(""),
    ];
    fields
        .iter()
        .filter_map(|f| matcher.fuzzy_match(f, query))
        .max()
}
