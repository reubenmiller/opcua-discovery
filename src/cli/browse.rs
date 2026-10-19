//! `browse` subcommand: the primary discovery command.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::Args;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use tokio::sync::mpsc;
use tracing::info;

use crate::cli::{ConnectionArgs, Encoding, FilterArgs, OutputArgs, TraversalArgs};
use crate::error::Result;
use crate::export::{NodeRecord, render_display, write_export};
use crate::filters::FilterSet;
use crate::opcua::{Connection, NamespaceTable};
use crate::scanner::{Engine, ScanConfig, resolve_root};
use crate::subscription;

/// Arguments for the `browse` command.
#[derive(Debug, Args)]
pub struct BrowseArgs {
    /// Connection / authentication options.
    #[command(flatten)]
    pub connection: ConnectionArgs,
    /// Traversal control.
    #[command(flatten)]
    pub traversal: TraversalArgs,
    /// Node filters.
    #[command(flatten)]
    pub filters: FilterArgs,
    /// Output / export options.
    #[command(flatten)]
    pub output: OutputArgs,

    /// Validate that subscribable nodes can actually be monitored.
    #[arg(long)]
    pub validate_subscription: bool,

    /// Reuse an existing `acquire` config: its `[connection]` supplies defaults
    /// and its `selection.node_ids` are pre-marked in the TUI. Saving (the `a`
    /// action) updates this file in place. CLI flags override config values.
    #[arg(long, value_name = "FILE", conflicts_with = "tedge_dot_config")]
    pub config: Option<std::path::PathBuf>,

    /// `[library] type` of a new tedge-dot point library. Defaults to the
    /// `type` of the `--tedge-dot-config` device.
    #[arg(long, value_name = "TYPE")]
    pub library_type: Option<String>,

    /// tedge-dot point library written by the TUI (`t`). When it exists, its
    /// points are pre-marked and saving merges into it.
    #[arg(long, value_name = "FILE", default_value = "point-library.toml")]
    pub library: std::path::PathBuf,
}

/// Run the `browse` command.
pub async fn run(mut args: BrowseArgs, cancel: Arc<AtomicBool>) -> Result<()> {
    // Optionally seed from an existing acquire config: reuse its connection and
    // pre-select its nodes in the TUI. CLI flags take precedence.
    let mut acquire_cfg: Option<crate::acquire::config::AcquireConfig> = None;
    let mut source_config: Option<std::path::PathBuf> = None;
    if let Some(path) = args.config.clone() {
        let cfg = crate::acquire::config::AcquireConfig::from_file(&path)?;
        cfg.connection.apply_to(&mut args.connection)?;
        acquire_cfg = Some(cfg);
        source_config = Some(path);
    }

    let conn = Connection::connect(&args.connection).await?;
    let session = conn.session().clone();

    let namespaces =
        Arc::new(crate::cli::load_namespaces(&session, &args.connection, conn.endpoint()).await?);
    let filters = Arc::new(FilterSet::compile(&args.filters, &namespaces)?);
    let root = resolve_root(&args.traversal.root)?;

    // Resolve the saved selection to live node ids now that namespaces are
    // known: each saved node's namespace URI is remapped to the server's
    // current index so pre-marking matches the live tree even if the server
    // reordered its namespace array.
    let mut selected_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut selected_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(cfg) = &acquire_cfg {
        for node in &cfg.selection.nodes {
            let id = crate::acquire::selection::remap_node_id(
                &node.id,
                node.namespace.as_deref(),
                &namespaces,
            )?;
            selected_ids.insert(id.to_string());
            if let Some(path) = &node.path
                && !path.is_empty()
            {
                selected_paths.insert(path.clone());
            }
        }
        // Legacy bare ids carry no URI: use them verbatim.
        selected_ids.extend(cfg.selection.node_ids.iter().cloned());
    }

    // Interactive explorer: opt-in, replaces streaming/export output entirely.
    if args.output.tui {
        // An existing point library: pre-mark its points (translated to the
        // server's current namespace indexes) so `t` edits it.
        if args.library.exists() {
            let text = std::fs::read_to_string(&args.library)?;
            let ids =
                crate::tedge_dot::library::node_ids(&text, &namespaces).map_err(|reason| {
                    crate::error::ScanError::Config {
                        path: args.library.clone(),
                        reason,
                    }
                })?;
            selected_ids.extend(ids);
        }
        // Write target: the source --config, else default.
        let acquire_file = source_config
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("acquire.toml"));
        let options = crate::tui::TuiOptions {
            root,
            page_size: args.traversal.page_size,
            export_file: args.output.file.clone(),
            strategy: args.traversal.strategy,
            workers: args.traversal.workers,
            max_children: args.traversal.max_children,
            retry: args.connection.retry,
            preview_values: args.output.preview_values,
            connection: gen_connection(&args.connection, conn.endpoint()),
            acquire_file,
            selected_ids,
            selected_paths,
            source_config,
            library_file: args.library.clone(),
            library_options: library_options(&args),
        };
        let result = crate::tui::run(session, namespaces, filters, options).await;
        conn.disconnect().await;
        return result;
    }

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

    // Watch mode repeats the scan periodically until interrupted.
    loop {
        scan_once(
            &session,
            &config,
            &filters,
            &namespaces,
            &args,
            cancel.clone(),
        )
        .await?;

        match args.output.watch {
            Some(interval) if !cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                eprintln!("\n— watching; next scan in {interval:?} (Ctrl+C to stop) —");
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = wait_for_cancel(cancel.clone()) => {}
                }
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
            }
            _ => break,
        }
    }

    conn.disconnect().await;
    Ok(())
}

/// Poll the cancel flag until it is set.
async fn wait_for_cancel(cancel: Arc<AtomicBool>) {
    while !cancel.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Build a connection summary for generating an `acquire` config from the TUI.
/// The password is intentionally never captured.
fn gen_connection(
    args: &ConnectionArgs,
    endpoint: &str,
) -> crate::acquire::generate::GenConnection {
    use crate::cli::{SecurityModeArg, SecurityPolicyArg};

    // The connection succeeded, so the pair is valid; write it explicitly.
    let (policy, mode) = args
        .effective_security()
        .unwrap_or((SecurityPolicyArg::None, SecurityModeArg::None));
    let security_policy = (policy != SecurityPolicyArg::None).then(|| policy.name().to_string());
    let security_mode = match mode {
        SecurityModeArg::None => None,
        SecurityModeArg::Sign => Some("Sign".to_string()),
        SecurityModeArg::SignAndEncrypt => Some("SignAndEncrypt".to_string()),
    };

    crate::acquire::generate::GenConnection {
        endpoint: endpoint.to_string(),
        trust_server: args.trust_server,
        security_policy,
        security_mode,
        allow_deprecated_security: args.allow_deprecated_security,
        user: args.user.clone(),
        pki_dir: args.pki_dir.clone(),
    }
}

/// Perform a single scan, then render or export the results.
async fn scan_once(
    session: &Arc<opcua::client::Session>,
    config: &Arc<ScanConfig>,
    filters: &Arc<FilterSet>,
    namespaces: &Arc<NamespaceTable>,
    args: &BrowseArgs,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let engine = Engine::new(
        session.clone(),
        config.clone(),
        filters.clone(),
        namespaces.clone(),
        cancel.clone(),
    );
    let stats = engine.stats();

    // Channel for streaming matched records.
    let (tx, mut rx) = mpsc::channel::<NodeRecord>(1024);

    // Run the engine on a task so we can consume results concurrently.
    let engine_task = tokio::spawn(async move { engine.run(tx).await });

    // A lightweight progress spinner on stderr (only when attached to a TTY).
    let progress = make_progress();

    let mut records: Vec<NodeRecord> = Vec::new();
    while let Some(record) = rx.recv().await {
        records.push(record);
        if let Some(pb) = &progress {
            let snap = stats.snapshot();
            pb.set_message(format!(
                "visited {}  matched {}  subscribable {}",
                snap.visited, snap.matched, snap.subscribable
            ));
            pb.tick();
        }
    }
    let _ = engine_task.await;
    if let Some(pb) = &progress {
        pb.finish_and_clear();
    }

    // Optionally validate subscriptions for subscribable nodes.
    if args.validate_subscription {
        subscription::validate_records(session, &mut records, cancel.clone()).await;
    }

    // Render or export.
    if args.output.output == Some(Encoding::TedgeDot) {
        write_library(session, &records, namespaces, args).await?;
    } else if let Some(encoding) = args.output.output {
        write_export(&mut records, encoding, args.output.file.as_deref())?;
    } else {
        render_display(&mut records, args.output.format)?;
    }

    let snap = stats.snapshot();
    info!(
        visited = snap.visited,
        matched = snap.matched,
        subscribable = snap.subscribable,
        pruned = snap.pruned,
        truncated = snap.truncated,
        errors = snap.errors,
        elapsed = ?snap.elapsed,
        "scan complete"
    );
    eprintln!(
        "\nvisited: {}  matched: {}  subscribable: {}  pruned: {}  truncated: {}  errors: {}  elapsed: {:.1?}",
        snap.visited,
        snap.matched,
        snap.subscribable,
        snap.pruned,
        snap.truncated,
        snap.errors,
        snap.elapsed
    );

    Ok(())
}

/// The options for writing a tedge-dot point library from `args`.
pub fn library_options(args: &BrowseArgs) -> crate::tedge_dot::library::LibraryOptions {
    crate::tedge_dot::library::LibraryOptions {
        library_type: args
            .library_type
            .clone()
            .or_else(|| args.connection.extras.device_type.clone()),
        endpoint: args.connection.endpoint.clone().unwrap_or_default(),
    }
}

/// Write `records` as a tedge-dot point library to `--file` (merging into an
/// existing library) or stdout, after discovering their custom datatypes and
/// method arguments. The file is left untouched on any error.
async fn write_library(
    session: &opcua::client::Session,
    records: &[NodeRecord],
    namespaces: &NamespaceTable,
    args: &BrowseArgs,
) -> Result<()> {
    use crate::error::ScanError;

    let file = args.output.file.as_deref();
    let existing = match file {
        Some(path) if path.exists() => Some(std::fs::read_to_string(path)?),
        _ => None,
    };
    let schema = crate::tedge_dot::schema::discover(session, records).await;
    let outcome = crate::tedge_dot::library::write(
        existing.as_deref(),
        records,
        &Default::default(),
        namespaces,
        &library_options(args),
        &schema,
    )
    .map_err(|reason| match file {
        Some(path) => ScanError::Config {
            path: path.to_path_buf(),
            reason,
        },
        None => ScanError::Export(reason),
    })?;

    match file {
        Some(path) => {
            std::fs::write(path, &outcome.text)?;
            eprintln!(
                "tedge-dot library {}: {} point(s) added, {} in total, {} node(s) skipped",
                path.display(),
                outcome.added,
                outcome.total,
                outcome.skipped.len()
            );
            if existing.is_some() {
                for s in &outcome.skipped {
                    eprintln!("  skipped {}  {}  ({})", s.node_id, s.browse_path, s.reason);
                }
            }
        }
        None => print!("{}", outcome.text),
    }
    Ok(())
}

/// Create a spinner progress bar, but only when stderr is an interactive
/// terminal (so piped/redirected output stays clean).
fn make_progress() -> Option<ProgressBar> {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let pb = ProgressBar::new_spinner();
    pb.set_draw_target(ProgressDrawTarget::stderr());
    pb.set_style(
        ProgressStyle::with_template("{spinner} scanning… {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    Some(pb)
}
