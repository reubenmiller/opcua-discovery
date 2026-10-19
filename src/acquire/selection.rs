//! Node selection for acquisition tasks.
//!
//! Selection combines two sources: explicit node ids listed in the config and
//! filter-based discovery that runs the scan engine and keeps matching variable
//! nodes. The result is a de-duplicated list of nodes with a friendly
//! measurement name for output.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use opcua::client::Session;
use opcua::types::NodeId;
use tokio::sync::mpsc;
use tracing::warn;

use crate::acquire::config::AcquireConfig;
use crate::acquire::config::NodeMode;
use crate::acquire::sink::SampleMeta;
use crate::error::{Result, ScanError};
use crate::export::NodeRecord;
use crate::filters::FilterSet;
use crate::filters::predicate::{builtin_datatype_name, canonical_data_type, is_subscribable};
use crate::opcua::attributes::read_node_attributes;
use crate::opcua::browse::browse_properties;
use crate::opcua::namespaces::NamespaceTable;
use crate::opcua::value::{eu_label_from_data_value, range_from_data_value, read_data_values};
use crate::scanner::{Engine, ScanConfig, resolve_root};

/// A selected node ready for sampling.
#[derive(Debug, Clone)]
pub struct SelectedNode {
    /// Measurement name used in output.
    pub name: String,
    /// Full browse path (for diagnostics).
    pub path: String,
    /// Node id.
    pub node_id: NodeId,
    /// Per-node acquisition mode (subscribe / poll / auto).
    pub mode: NodeMode,
    /// Whether the server reports the node as monitorable (Variable + readable).
    /// Used to resolve [`NodeMode::Auto`] into subscribe or poll.
    pub subscribable: bool,
    /// Descriptive context attached to every emitted sample.
    pub meta: SampleMeta,
}

/// The outcome of resolving a selection: the concrete nodes plus any browse
/// paths that were reconstructed from the server and should be cached back into
/// the config file.
#[derive(Debug, Default)]
pub struct Resolved {
    /// Resolved nodes ready for sampling.
    pub nodes: Vec<SelectedNode>,
    /// `(config node id, reconstructed path)` pairs for explicit nodes whose
    /// path was discovered (i.e. not already cached in the config).
    pub path_updates: Vec<(String, String)>,
}

/// Resolve the configured selection into a concrete list of nodes.
///
/// `endpoint` is the connected endpoint URL, attached to every node as the
/// source endpoint; the optional source name comes from `[connection] name`.
pub async fn resolve(
    session: &Arc<Session>,
    config: &AcquireConfig,
    namespaces: &Arc<NamespaceTable>,
    endpoint: &str,
    cancel: Arc<AtomicBool>,
) -> Result<Resolved> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut nodes: Vec<SelectedNode> = Vec::new();

    // 1. Explicit nodes (rich `[[selection.node]]` entries) + legacy `node_ids`.
    let (explicit, path_updates) = resolve_explicit(
        session,
        &config.selection.nodes,
        &config.selection.node_ids,
        namespaces,
    )
    .await?;
    for node in explicit {
        if seen.insert(node.node_id.clone()) {
            nodes.push(node);
        }
    }

    // 2. Filter-based discovery (only when filters are present).
    if has_filters(config) {
        let discovered = resolve_filtered(session, config, namespaces, cancel).await?;
        for node in discovered {
            if seen.insert(node.node_id.clone()) {
                nodes.push(node);
            }
        }
    }

    // 3. Fold EURange / EngineeringUnits properties into their parent nodes and
    //    drop any that were selected as standalone nodes.
    enrich_metadata(session, &mut nodes).await?;

    // 4. Stamp the data source (endpoint + friendly name) onto every node.
    let source_name = config.connection.name.clone();
    for node in &mut nodes {
        node.meta.source_endpoint = Some(endpoint.to_string());
        node.meta.source_name = source_name.clone();
    }

    Ok(Resolved {
        nodes,
        path_updates,
    })
}

fn has_filters(config: &AcquireConfig) -> bool {
    let s = &config.selection;
    !s.include.is_empty()
        || !s.include_regex.is_empty()
        || !s.node_class.is_empty()
        || !s.datatype.is_empty()
        || s.root.is_some()
}

async fn resolve_explicit(
    session: &Session,
    nodes: &[crate::acquire::config::NodeSelector],
    legacy_ids: &[String],
    namespaces: &NamespaceTable,
) -> Result<(Vec<SelectedNode>, Vec<(String, String)>)> {
    if nodes.is_empty() && legacy_ids.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    // Parse + remap every selector to a concrete (live) NodeId, keeping any
    // configured friendly name, cached path, and original config id alongside.
    struct Parsed {
        id: NodeId,
        configured_name: Option<String>,
        cached_path: Option<String>,
        config_id: Option<String>,
        mode: NodeMode,
        key: Option<String>,
    }
    let mut parsed: Vec<Parsed> = Vec::with_capacity(nodes.len() + legacy_ids.len());
    for node in nodes {
        let id = remap_node_id(&node.id, node.namespace.as_deref(), namespaces)?;
        parsed.push(Parsed {
            id,
            configured_name: node.name.clone(),
            cached_path: node.path.clone().filter(|s| !s.is_empty()),
            config_id: Some(node.id.clone()),
            mode: node.mode,
            key: node.key.clone(),
        });
    }
    for id in legacy_ids {
        // Legacy bare ids carry no namespace URI: use the literal index as-is.
        let node_id = remap_node_id(id, None, namespaces)?;
        parsed.push(Parsed {
            id: node_id,
            configured_name: None,
            cached_path: None,
            config_id: None,
            mode: NodeMode::Auto,
            key: None,
        });
    }

    let ids: Vec<NodeId> = parsed.iter().map(|p| p.id.clone()).collect();
    let attrs = read_node_attributes(session, &ids).await?;
    let mut out = Vec::with_capacity(parsed.len());
    let mut path_updates = Vec::new();
    for (p, attr) in parsed.into_iter().zip(attrs) {
        let name = p
            .configured_name
            .or_else(|| attr.display_name.clone())
            .unwrap_or_else(|| p.id.to_string());

        // Skip nodes the server reports as not currently readable: reading them
        // only ever yields `BadNotReadable`, so they cannot be sampled by any
        // mode. Warn so the user understands why a configured node is dropped.
        if attr.is_unreadable_variable() {
            warn!(
                node_id = %p.id,
                node = %name,
                "skipping non-readable node (AccessLevel has no CurrentRead); it cannot be sampled"
            );
            continue;
        }

        // Resolve the full browse path: use the cached value when present,
        // otherwise reconstruct it from the server and remember it for caching.
        let browse_path = match p.cached_path {
            Some(path) => Some(path),
            None => {
                let leaf = attr
                    .browse_name
                    .clone()
                    .or_else(|| attr.display_name.clone())
                    .unwrap_or_else(|| p.id.to_string());
                let reconstructed =
                    crate::opcua::browse::reconstruct_browse_path(session, &p.id, &leaf).await?;
                if let (Some(path), Some(config_id)) = (&reconstructed, &p.config_id) {
                    path_updates.push((config_id.clone(), path.clone()));
                }
                reconstructed
            }
        };

        let meta = SampleMeta {
            data_type: data_type_string(
                attr.data_type.as_ref().and_then(builtin_datatype_name),
                attr.value_rank,
            ),
            engineering_units: None,
            browse_name: attr.browse_name.clone(),
            display_name: attr.display_name.clone(),
            browse_path: browse_path.clone(),
            namespace: namespaces.uri(p.id.namespace).map(str::to_string),
            eu_range: None,
            source_endpoint: None,
            source_name: None,
            key: p.key.clone(),
        };
        out.push(SelectedNode {
            path: browse_path.unwrap_or_else(|| name_or_id(&attr.display_name, &p.id)),
            name,
            node_id: p.id,
            mode: p.mode,
            subscribable: is_subscribable(&attr),
            meta,
        });
    }
    Ok((out, path_updates))
}

/// Resolve a node id string to a concrete [`NodeId`], remapping the namespace
/// index from a stable URI when one is supplied.
///
/// When `ns_uri` is `Some`, the index encoded in `id` is replaced with the
/// server's current index for that URI, so the selection survives the server
/// reordering its namespace array. An error is returned if the URI is not
/// present on the server (better to fail loudly than silently sample the wrong
/// node). When `ns_uri` is `None` (legacy ids), the id is used verbatim.
pub(crate) fn remap_node_id(
    id: &str,
    ns_uri: Option<&str>,
    namespaces: &NamespaceTable,
) -> Result<NodeId> {
    let parsed = NodeId::from_str(id)
        .map_err(|_| ScanError::InvalidArgument(format!("invalid node id '{id}'")))?;
    match ns_uri {
        Some(uri) if !uri.is_empty() => {
            let index = namespaces.index(uri).ok_or_else(|| {
                ScanError::InvalidArgument(format!(
                    "namespace '{uri}' (for node '{id}') is not present on the server"
                ))
            })?;
            Ok(NodeId::new(index, parsed.identifier.clone()))
        }
        _ => Ok(parsed),
    }
}

fn name_or_id(display_name: &Option<String>, id: &NodeId) -> String {
    display_name.clone().unwrap_or_else(|| id.to_string())
}

async fn resolve_filtered(
    session: &Arc<Session>,
    config: &AcquireConfig,
    namespaces: &Arc<NamespaceTable>,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<SelectedNode>> {
    // Only variables can be sampled.
    let mut filter_args = config.filter_args()?;
    filter_args.subscribable = true;
    let filters = Arc::new(FilterSet::compile(&filter_args, namespaces)?);
    let root = resolve_root(&config.selection.root)?;

    let scan_config = Arc::new(ScanConfig {
        root,
        depth: config.selection.depth()?,
        strategy: crate::cli::Strategy::Bfs,
        workers: 8,
        page_size: 200,
        max_children: None,
        retry: config.connection.retry.unwrap_or(3),
        preview_values: false,
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

    let mut nodes = Vec::new();
    while let Some(record) = rx.recv().await {
        match NodeId::from_str(&record.node_id) {
            Ok(node_id) => {
                let meta = SampleMeta {
                    data_type: data_type_string(record.datatype.as_deref(), record.value_rank),
                    engineering_units: None,
                    browse_name: non_empty(&record.browse_name),
                    display_name: non_empty(&record.display_name),
                    browse_path: non_empty(&record.browse_path),
                    namespace: non_empty(&record.namespace_uri),
                    eu_range: None,
                    source_endpoint: None,
                    source_name: None,
                    key: None,
                };
                nodes.push(SelectedNode {
                    name: measurement_name(&record),
                    path: record.browse_path,
                    node_id,
                    // Filtered discovery only keeps subscribable variables (the
                    // scan applies the subscribable filter), so default to auto
                    // routing which will prefer subscribe for these.
                    mode: NodeMode::Auto,
                    subscribable: true,
                    meta,
                });
            }
            Err(_) => warn!(node_id = %record.node_id, "skipping unparseable node id"),
        }
    }
    let _ = engine_task.await;
    Ok(nodes)
}

/// `Some(s)` when the string is non-empty, else `None`.
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Build the normalized `dataType` string for output from an OPC UA builtin
/// type name and value rank. Scalar primitives map via [`canonical_data_type`];
/// arrays (`value_rank >= 0`) get a `[]` suffix. Non-builtin or unknown types
/// yield `None` (rendered as `null`).
fn data_type_string(builtin_name: Option<&str>, value_rank: Option<i32>) -> Option<String> {
    let canonical = canonical_data_type(builtin_name?)?;
    let is_array = value_rank.map(|r| r >= 0).unwrap_or(false);
    if is_array {
        Some(format!("{canonical}[]"))
    } else {
        Some(canonical.to_string())
    }
}

/// Property kind folded into a parent node's metadata.
#[derive(Debug, Clone, Copy)]
enum PropKind {
    EuRange,
    EngineeringUnits,
}

/// Classify a selected node as an `EURange` / `EngineeringUnits` property based
/// on its browse name (falling back to the last browse-path segment when the
/// browse name was not read).
fn property_kind(node: &SelectedNode) -> Option<PropKind> {
    let name = node
        .meta
        .browse_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| node.path.rsplit('/').next())
        .unwrap_or("");
    match name {
        "EURange" => Some(PropKind::EuRange),
        "EngineeringUnits" => Some(PropKind::EngineeringUnits),
        _ => None,
    }
}

/// Discover each node's `EURange` / `EngineeringUnits` properties, fold their
/// decoded values into the parent's [`SampleMeta`], and drop any property that
/// was also selected as a standalone node (so it is not emitted on its own).
async fn enrich_metadata(session: &Session, nodes: &mut Vec<SelectedNode>) -> Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }

    let ids: Vec<NodeId> = nodes.iter().map(|n| n.node_id.clone()).collect();
    let property_lists = browse_properties(session, &ids).await?;

    // Plan the property value reads, remembering which parent + kind each one
    // belongs to, and which node ids end up folded away.
    let mut to_read: Vec<NodeId> = Vec::new();
    let mut targets: Vec<(usize, PropKind)> = Vec::new();
    let mut folded: HashSet<NodeId> = HashSet::new();

    // (A) Top-down: discover each selected node's EURange / EngineeringUnits
    // child properties and fold their values into that parent.
    for (idx, refs) in property_lists.iter().enumerate() {
        for reference in refs {
            let browse_name = reference.browse_name.name.to_string();
            let kind = match browse_name.as_str() {
                "EURange" => PropKind::EuRange,
                "EngineeringUnits" => PropKind::EngineeringUnits,
                _ => continue,
            };
            let prop_id = reference.node_id.node_id.clone();
            folded.insert(prop_id.clone());
            to_read.push(prop_id);
            targets.push((idx, kind));
        }
    }

    // (B) Bottom-up: some configs list an EURange / EngineeringUnits property
    // node itself as a standalone selection. Such a node is metadata, not a
    // measurement, and reading it on its own only yields a null value. Fold its
    // value into the parent variable (matched by browse path) when that parent
    // is also selected, and always drop it so it is not sampled standalone.
    let path_index: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.path.clone(), i))
        .collect();
    for node in nodes.iter() {
        let Some(kind) = property_kind(node) else {
            continue;
        };
        folded.insert(node.node_id.clone());
        if let Some((parent_path, _)) = node.path.rsplit_once('/')
            && let Some(&parent_idx) = path_index.get(parent_path)
        {
            to_read.push(node.node_id.clone());
            targets.push((parent_idx, kind));
        }
    }

    if !to_read.is_empty() {
        let values = read_data_values(session, &to_read).await?;
        for ((idx, kind), dv) in targets.into_iter().zip(values.iter()) {
            match kind {
                PropKind::EuRange => {
                    if let Some(range) = range_from_data_value(dv) {
                        nodes[idx].meta.eu_range = Some(range);
                    }
                }
                PropKind::EngineeringUnits => {
                    if let Some(label) = eu_label_from_data_value(dv) {
                        nodes[idx].meta.engineering_units = Some(label);
                    }
                }
            }
        }
    }

    // Remove nodes that are themselves a folded property of another selection.
    if !folded.is_empty() {
        nodes.retain(|n| !folded.contains(&n.node_id));
    }
    Ok(())
}

/// Choose a measurement name for a discovered record (display name, else the
/// last browse path segment, else the node id).
fn measurement_name(record: &NodeRecord) -> String {
    if !record.display_name.is_empty() {
        return record.display_name.clone();
    }
    record
        .browse_path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| record.node_id.clone())
}
