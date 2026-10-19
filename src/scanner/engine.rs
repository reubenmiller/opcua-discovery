//! Concurrent, cycle-safe traversal engine.
//!
//! The engine expands the address space starting from a root node using a pool
//! of workers pulling from a shared work queue. BFS pops from the front of the
//! queue, DFS from the back. Results are streamed to the caller over a channel
//! so memory stays low even on very large servers.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use opcua::client::Session;
use opcua::types::{
    BrowseDescription, BrowseDirection, BrowseResult, BrowseResultMask, ByteString, NodeId,
    ReferenceDescription, ReferenceTypeId, StatusCode,
};
use tokio::sync::{Mutex, Notify, mpsc};
use tracing::{debug, trace};

use crate::cli::{Depth, Strategy};
use crate::error::Result;
use crate::export::record::NodeRecord;
use crate::filters::FilterSet;
use crate::opcua::attributes::read_node_attributes;
use crate::opcua::namespaces::NamespaceTable;
use crate::opcua::node_class_name;
use crate::opcua::value::read_values;

use super::stats::ScanStats;

/// Configuration controlling a single scan.
pub struct ScanConfig {
    /// Starting node id.
    pub root: NodeId,
    /// Maximum traversal depth.
    pub depth: Depth,
    /// Traversal strategy.
    pub strategy: Strategy,
    /// Number of concurrent workers.
    pub workers: usize,
    /// References requested per browse call.
    pub page_size: u32,
    /// Per-branch child cap.
    pub max_children: Option<usize>,
    /// Retry attempts for transient browse failures.
    pub retry: u32,
    /// Whether to read current values for matched variable nodes.
    pub preview_values: bool,
}

/// A node queued for expansion.
#[derive(Debug, Clone)]
struct Pending {
    node_id: NodeId,
    path: String,
    depth: u32,
}

/// Shared state for the worker pool.
struct Shared {
    queue: Mutex<std::collections::VecDeque<Pending>>,
    active: AtomicUsize,
    notify: Notify,
    cancel: Arc<AtomicBool>,
    visited: StdMutex<HashSet<NodeId>>,
    dfs: bool,
}

/// The traversal engine.
pub struct Engine {
    session: Arc<Session>,
    config: Arc<ScanConfig>,
    filters: Arc<FilterSet>,
    namespaces: Arc<NamespaceTable>,
    stats: Arc<ScanStats>,
    cancel: Arc<AtomicBool>,
}

impl Engine {
    /// Create a new engine.
    pub fn new(
        session: Arc<Session>,
        config: Arc<ScanConfig>,
        filters: Arc<FilterSet>,
        namespaces: Arc<NamespaceTable>,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            session,
            config,
            filters,
            namespaces,
            stats: Arc::new(ScanStats::new()),
            cancel,
        }
    }

    /// Shared statistics handle (for progress reporting).
    pub fn stats(&self) -> Arc<ScanStats> {
        self.stats.clone()
    }

    /// Run the scan, streaming matched records to `sink`. Returns when traversal
    /// completes or is cancelled.
    pub async fn run(&self, sink: mpsc::Sender<NodeRecord>) -> Result<()> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(std::collections::VecDeque::new()),
            active: AtomicUsize::new(0),
            notify: Notify::new(),
            cancel: self.cancel.clone(),
            visited: StdMutex::new(HashSet::new()),
            dfs: matches!(self.config.strategy, Strategy::Dfs),
        });

        // Seed with the root node (the root itself is the traversal anchor and
        // is not emitted; its children form the top level of the browse path).
        shared.queue.lock().await.push_back(Pending {
            node_id: self.config.root.clone(),
            path: String::new(),
            depth: 0,
        });

        let worker_count = self.config.workers.max(1);
        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let worker = Worker {
                session: self.session.clone(),
                config: self.config.clone(),
                filters: self.filters.clone(),
                namespaces: self.namespaces.clone(),
                stats: self.stats.clone(),
                shared: shared.clone(),
                sink: sink.clone(),
            };
            handles.push(tokio::spawn(async move { worker.run().await }));
        }
        drop(sink);

        for h in handles {
            let _ = h.await;
        }
        Ok(())
    }
}

/// A single worker pulling from the shared queue.
struct Worker {
    session: Arc<Session>,
    config: Arc<ScanConfig>,
    filters: Arc<FilterSet>,
    namespaces: Arc<NamespaceTable>,
    stats: Arc<ScanStats>,
    shared: Arc<Shared>,
    sink: mpsc::Sender<NodeRecord>,
}

impl Worker {
    async fn run(self) {
        loop {
            if self.shared.cancel.load(Ordering::Relaxed) {
                break;
            }

            let next = {
                let mut q = self.shared.queue.lock().await;
                let item = if self.shared.dfs {
                    q.pop_back()
                } else {
                    q.pop_front()
                };
                if item.is_some() {
                    self.shared.active.fetch_add(1, Ordering::SeqCst);
                }
                item
            };

            match next {
                Some(pending) => {
                    self.expand(pending).await;
                    self.shared.active.fetch_sub(1, Ordering::SeqCst);
                    self.shared.notify.notify_waiters();
                }
                None => {
                    if self.shared.active.load(Ordering::SeqCst) == 0 {
                        self.shared.notify.notify_waiters();
                        break;
                    }
                    tokio::select! {
                        _ = self.shared.notify.notified() => {}
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                    }
                }
            }
        }
    }

    /// Expand a single node: browse children, filter, emit, and enqueue.
    async fn expand(&self, pending: Pending) {
        // Cycle protection: never browse the same node twice.
        {
            let mut visited = self.shared.visited.lock().unwrap();
            if !visited.insert(pending.node_id.clone()) {
                trace!(node = %pending.node_id, "skipping already-visited node");
                return;
            }
        }

        let (refs, truncated) = match self.browse_children(&pending.node_id).await {
            Ok(v) => v,
            Err(status) => {
                debug!(node = %pending.node_id, %status, "browse failed; continuing");
                self.stats.inc_errors();
                return;
            }
        };

        if truncated {
            self.stats.inc_truncated();
        }
        self.stats.add_visited(refs.len() as u64);

        // Batch-read attributes for all children in one service call.
        let child_ids: Vec<NodeId> = refs.iter().map(|r| r.node_id.node_id.clone()).collect();
        let attrs = read_node_attributes(&self.session, &child_ids)
            .await
            .unwrap_or_else(|_| vec![Default::default(); child_ids.len()]);

        let child_depth = pending.depth + 1;
        for (reference, attr) in refs.into_iter().zip(attrs) {
            if self.shared.cancel.load(Ordering::Relaxed) {
                return;
            }
            let node_id = reference.node_id.node_id.clone();
            let segment = path_segment(&reference, &node_id);
            let path = if pending.path.is_empty() {
                segment.clone()
            } else {
                format!("{}/{}", pending.path, segment)
            };

            let decision = self.filters.path_decision(&path);
            if decision.pruned {
                self.stats.inc_pruned();
                continue;
            }

            // Emit if the path is included and attribute predicates pass.
            if decision.path_included
                && self
                    .filters
                    .attributes_match(&node_id, &attr, &self.namespaces)
            {
                let record = self
                    .build_record(&reference, &node_id, &path, &attr, &pending.node_id)
                    .await;
                if crate::filters::is_subscribable(&attr) {
                    self.stats.inc_subscribable();
                }
                self.stats.inc_matched();
                if self.sink.send(record).await.is_err() {
                    // Receiver dropped: stop work.
                    self.shared.cancel.store(true, Ordering::Relaxed);
                    return;
                }
            }

            // Descend if path filters allow and depth permits browsing the child.
            if decision.descend && self.config.depth.allows_children_at(child_depth) {
                let mut q = self.shared.queue.lock().await;
                q.push_back(Pending {
                    node_id,
                    path,
                    depth: child_depth,
                });
            }
        }
        self.shared.notify.notify_waiters();
    }

    /// Build a full export record for a matched node.
    async fn build_record(
        &self,
        reference: &ReferenceDescription,
        node_id: &NodeId,
        path: &str,
        attr: &crate::opcua::attributes::NodeAttributes,
        parent: &NodeId,
    ) -> NodeRecord {
        let datatype = attr.data_type.as_ref().map(|dt| {
            crate::filters::predicate::builtin_datatype_name(dt)
                .map(|s| s.to_string())
                .unwrap_or_else(|| dt.to_string())
        });

        let mut record = NodeRecord {
            browse_path: path.to_string(),
            display_name: reference.display_name.text.to_string(),
            browse_name: reference.browse_name.name.to_string(),
            node_id: node_id.to_string(),
            namespace_uri: self.namespaces.display(node_id.namespace),
            node_class: node_class_name(reference.node_class).to_string(),
            datatype,
            value_rank: attr.value_rank,
            access_level: attr.access_level,
            user_access_level: attr.user_access_level,
            historizing: attr.historizing,
            subscribable: crate::filters::is_subscribable(attr),
            validation_status: None,
            current_value: None,
            engineering_units: None,
            description: attr.description.clone(),
            parent_node_id: (reference.node_class == opcua::types::NodeClass::Method)
                .then(|| parent.to_string()),
        };

        if self.config.preview_values
            && record.subscribable
            && let Ok(values) = read_values(&self.session, std::slice::from_ref(node_id)).await
            && let Some(snapshot) = values.into_iter().next()
        {
            record.current_value = snapshot.value;
        }

        record
    }

    /// Browse all hierarchical children of a node, following continuation points
    /// and honouring retry / max-children.
    async fn browse_children(
        &self,
        node_id: &NodeId,
    ) -> std::result::Result<(Vec<ReferenceDescription>, bool), StatusCode> {
        let desc = BrowseDescription {
            node_id: node_id.clone(),
            browse_direction: BrowseDirection::Forward,
            reference_type_id: ReferenceTypeId::HierarchicalReferences.into(),
            include_subtypes: true,
            node_class_mask: 0,
            result_mask: BrowseResultMask::All as u32,
        };

        let mut results = self
            .browse_with_retry(&[desc], self.config.page_size)
            .await?;
        let mut result = results
            .drain(..)
            .next()
            .ok_or(StatusCode::BadUnexpectedError)?;

        let mut refs = Vec::new();
        let mut truncated = false;
        loop {
            if !result.status_code.is_good() && result.references.is_none() {
                return Err(result.status_code);
            }
            if let Some(rs) = result.references.take() {
                for r in rs {
                    refs.push(r);
                    if let Some(max) = self.config.max_children
                        && refs.len() > max
                    {
                        truncated = true;
                        break;
                    }
                }
            }
            if truncated || result.continuation_point.is_null() {
                break;
            }
            let cp = result.continuation_point.clone();
            let mut next = self.browse_next_with_retry(&[cp]).await?;
            result = next
                .drain(..)
                .next()
                .ok_or(StatusCode::BadUnexpectedError)?;
        }

        if let Some(max) = self.config.max_children
            && refs.len() > max
        {
            refs.truncate(max);
            truncated = true;
        }
        Ok((refs, truncated))
    }

    async fn browse_with_retry(
        &self,
        desc: &[BrowseDescription],
        page_size: u32,
    ) -> std::result::Result<Vec<BrowseResult>, StatusCode> {
        let mut attempt = 0;
        loop {
            match self.session.browse(desc, page_size, None).await {
                Ok(r) => return Ok(r),
                Err(status) if is_transient(status) && attempt < self.config.retry => {
                    backoff(attempt).await;
                    attempt += 1;
                }
                Err(status) => return Err(status),
            }
        }
    }

    async fn browse_next_with_retry(
        &self,
        cps: &[ByteString],
    ) -> std::result::Result<Vec<BrowseResult>, StatusCode> {
        let mut attempt = 0;
        loop {
            match self.session.browse_next(false, cps).await {
                Ok(r) => return Ok(r),
                Err(status) if is_transient(status) && attempt < self.config.retry => {
                    backoff(attempt).await;
                    attempt += 1;
                }
                Err(status) => return Err(status),
            }
        }
    }
}

/// Derive a browse-path segment from a reference, preferring the browse name.
fn path_segment(reference: &ReferenceDescription, node_id: &NodeId) -> String {
    let browse = reference.browse_name.name.to_string();
    if !browse.is_empty() {
        return browse;
    }
    let display = reference.display_name.text.to_string();
    if !display.is_empty() {
        return display;
    }
    node_id.to_string()
}

/// Whether a status code represents a transient, retryable failure.
fn is_transient(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BadTimeout
            | StatusCode::BadTooManyOperations
            | StatusCode::BadServerTooBusy
            | StatusCode::BadNoContinuationPoints
            | StatusCode::BadServerNotConnected
    )
}

/// Exponential backoff with a small cap.
async fn backoff(attempt: u32) {
    let ms = 100u64.saturating_mul(1u64 << attempt.min(5));
    tokio::time::sleep(Duration::from_millis(ms.min(2000))).await;
}
