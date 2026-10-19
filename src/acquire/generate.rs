//! Generate a runnable `acquire` TOML config from an interactive selection.
//!
//! This is the write-side counterpart to [`crate::acquire::config`] (which only
//! deserializes). The TUI uses it to turn a set of selected nodes into a ready
//! config that `opcua-scan acquire --config <file>` can run directly.
//!
//! Secrets are never written: a `user` is recorded but the password is left as a
//! commented hint, so the generated file is safe to share or commit.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::error::{Result, ScanError};

/// The acquisition mode chosen for the generated task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenMode {
    /// Cyclic reading at a fixed interval.
    Poll,
    /// Real-time subscription with the interval as the publishing interval.
    Subscribe,
}

impl GenMode {
    /// The TOML token for this mode (`poll` / `subscribe`).
    fn token(self) -> &'static str {
        match self {
            GenMode::Poll => "poll",
            GenMode::Subscribe => "subscribe",
        }
    }
}

/// Connection details to embed in the generated `[connection]` table.
#[derive(Debug, Clone, Default)]
pub struct GenConnection {
    /// Endpoint URL.
    pub endpoint: String,
    /// Whether the server certificate is trusted automatically.
    pub trust_server: bool,
    /// Security policy name, if not the default `None`.
    pub security_policy: Option<String>,
    /// Security mode name, if not the default `None`.
    pub security_mode: Option<String>,
    /// Whether a deprecated security policy is allowed.
    pub allow_deprecated_security: bool,
    /// Username, if user/password auth is in use (password is NOT written).
    pub user: Option<String>,
    /// PKI directory, if set.
    pub pki_dir: Option<PathBuf>,
}

/// A selected node to write into the generated config. Carries the namespace
/// URI (so the numeric index can be remapped if the server reorders its
/// namespace array) and a friendly name for recognizability.
#[derive(Debug, Clone, Default)]
pub struct GenNode {
    /// Node id string (e.g. `ns=4;i=1274`).
    pub id: String,
    /// Namespace URI, when known (empty/`None` for the base namespace).
    pub namespace: Option<String>,
    /// Friendly display name.
    pub name: Option<String>,
    /// Cached full browse path, when known.
    pub path: Option<String>,
    /// Explicit per-node mode. `None` follows the config default (auto).
    pub mode: Option<GenMode>,
}

#[derive(Debug, Serialize)]
struct GeneratedConfig {
    connection: GenConnectionToml,
    #[serde(skip_serializing_if = "Option::is_none")]
    acquire: Option<GenAcquireToml>,
    selection: GenSelectionToml,
    output: GenOutputToml,
    task: Vec<GenTaskToml>,
}

#[derive(Debug, Serialize)]
struct GenAcquireToml {
    default_mode: String,
}

#[derive(Debug, Serialize)]
struct GenConnectionToml {
    endpoint: String,
    #[serde(skip_serializing_if = "is_false")]
    trust_server: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    security_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    security_mode: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    allow_deprecated_security: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pki_dir: Option<String>,
}

#[derive(Debug, Serialize)]
struct GenSelectionToml {
    node: Vec<GenNodeToml>,
}

#[derive(Debug, Serialize)]
struct GenNodeToml {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
}

impl From<&GenNode> for GenNodeToml {
    fn from(n: &GenNode) -> Self {
        Self {
            id: n.id.clone(),
            namespace: n.namespace.clone().filter(|s| !s.is_empty()),
            name: n.name.clone().filter(|s| !s.is_empty()),
            path: n.path.clone().filter(|s| !s.is_empty()),
            mode: n.mode.map(|m| m.token().to_string()),
        }
    }
}

/// Stable sort key for a selected node, so the written selection has a
/// deterministic order regardless of how the caller collected the nodes.
/// Without this, re-saving an unchanged selection can reorder the
/// `[[selection.node]]` entries (the TUI gathers them from unordered
/// collections), producing spurious diffs.
fn sort_key(n: &GenNode) -> (String, String) {
    (n.namespace.clone().unwrap_or_default(), n.id.clone())
}

#[derive(Debug, Serialize)]
struct GenOutputToml {
    format: String,
}

#[derive(Debug, Serialize)]
struct GenTaskToml {
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publishing_interval: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Build a runnable acquire TOML document from a connection and the selected
/// nodes.
///
/// `default_mode` is the acquisition mode applied to nodes that do not carry an
/// explicit per-node mode; `interval` is the poll interval (and, for the
/// subscribe task, the publishing interval). Task blocks are emitted for
/// whichever modes the nodes actually resolve to, so a mixed selection produces
/// both a `poll` and a `subscribe` task. An `[acquire]` table records the
/// default whenever any node relies on it.
pub fn generate_toml(
    conn: &GenConnection,
    nodes: Vec<GenNode>,
    default_mode: GenMode,
    interval: Duration,
) -> Result<String> {
    let interval_str = humantime::format_duration(interval).to_string();

    // Stable ordering so an unchanged selection always serializes identically.
    let mut nodes = nodes;
    nodes.sort_by_key(sort_key);

    // Resolve which modes are in play to decide which task blocks to emit.
    let resolved = |n: &GenNode| n.mode.unwrap_or(default_mode);
    let needs_poll = nodes.iter().any(|n| resolved(n) == GenMode::Poll);
    let needs_subscribe = nodes.iter().any(|n| resolved(n) == GenMode::Subscribe);
    let has_auto = nodes.iter().any(|n| n.mode.is_none());

    let mut task = Vec::new();
    if needs_poll {
        task.push(GenTaskToml {
            mode: "poll".to_string(),
            interval: Some(interval_str.clone()),
            publishing_interval: None,
        });
    }
    if needs_subscribe {
        task.push(GenTaskToml {
            mode: "subscribe".to_string(),
            interval: None,
            publishing_interval: Some(interval_str.clone()),
        });
    }
    // Fall back to a single task for the default mode when the selection is
    // empty, so the generated config is still runnable.
    if task.is_empty() {
        task.push(match default_mode {
            GenMode::Poll => GenTaskToml {
                mode: "poll".to_string(),
                interval: Some(interval_str.clone()),
                publishing_interval: None,
            },
            GenMode::Subscribe => GenTaskToml {
                mode: "subscribe".to_string(),
                interval: None,
                publishing_interval: Some(interval_str.clone()),
            },
        });
    }

    // Record the default only when at least one node relies on it.
    let acquire = has_auto.then(|| GenAcquireToml {
        default_mode: default_mode.token().to_string(),
    });

    let config = GeneratedConfig {
        connection: GenConnectionToml {
            endpoint: conn.endpoint.clone(),
            trust_server: conn.trust_server,
            security_policy: conn.security_policy.clone(),
            security_mode: conn.security_mode.clone(),
            allow_deprecated_security: conn.allow_deprecated_security,
            user: conn.user.clone(),
            pki_dir: conn.pki_dir.as_ref().map(|p| p.display().to_string()),
        },
        acquire,
        selection: GenSelectionToml {
            node: nodes.iter().map(GenNodeToml::from).collect(),
        },
        output: GenOutputToml {
            format: "tedge".to_string(),
        },
        task,
    };

    let body = toml::to_string_pretty(&config).map_err(|e| ScanError::Export(e.to_string()))?;

    let mut out = String::new();
    out.push_str("# Generated by `opcua-scan browse --tui` (press 'a').\n");
    out.push_str("# Run with: opcua-scan acquire --config <this file>\n");
    if conn.user.is_some() {
        out.push_str("# NOTE: set the password via `password = \"env:OPCUA_PASSWORD\"` under [connection].\n");
    }
    out.push('\n');
    out.push_str(&body);
    Ok(out)
}

/// Update only the selection of an existing acquire config, preserving
/// everything else (connection, tasks, output, comments, and formatting). The
/// selection is written as `[[selection.node]]` tables; any legacy `node_ids`
/// array is removed (migrated). Used when re-saving a config loaded into the
/// browse TUI.
pub fn update_selection_in_toml(original: &str, nodes: &[GenNode]) -> Result<String> {
    use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

    let mut doc: DocumentMut = original
        .parse()
        .map_err(|e: toml_edit::TomlError| ScanError::Export(e.to_string()))?;

    // Ensure a `[selection]` table exists.
    if !doc.contains_key("selection") {
        doc["selection"] = Item::Table(Table::new());
    }
    let selection = doc["selection"]
        .as_table_mut()
        .ok_or_else(|| ScanError::Export("`selection` is not a table".to_string()))?;

    // Migrate away from the legacy flat array.
    selection.remove("node_ids");

    // Stable ordering so re-saving an unchanged selection does not reorder the
    // `[[selection.node]]` entries.
    let mut nodes: Vec<&GenNode> = nodes.iter().collect();
    nodes.sort_by_key(|n| sort_key(n));

    let mut tables = ArrayOfTables::new();
    for node in nodes {
        let toml = GenNodeToml::from(node);
        let mut t = Table::new();
        t["id"] = toml_edit::value(toml.id);
        if let Some(ns) = toml.namespace {
            t["namespace"] = toml_edit::value(ns);
        }
        if let Some(name) = toml.name {
            t["name"] = toml_edit::value(name);
        }
        if let Some(path) = toml.path {
            t["path"] = toml_edit::value(path);
        }
        if let Some(mode) = toml.mode {
            t["mode"] = toml_edit::value(mode);
        }
        tables.push(t);
    }
    selection.insert("node", Item::ArrayOfTables(tables));

    Ok(doc.to_string())
}

/// Add a cached `path` to existing `[[selection.node]]` entries, matching by the
/// node's `id` field. Preserves all other content (formatting, comments, tasks).
/// `updates` is a list of `(node id, browse path)` pairs.
pub fn update_node_paths_in_toml(original: &str, updates: &[(String, String)]) -> Result<String> {
    use toml_edit::{DocumentMut, value};

    if updates.is_empty() {
        return Ok(original.to_string());
    }

    let mut doc: DocumentMut = original
        .parse()
        .map_err(|e: toml_edit::TomlError| ScanError::Export(e.to_string()))?;

    let Some(nodes) = doc
        .get_mut("selection")
        .and_then(|s| s.get_mut("node"))
        .and_then(|n| n.as_array_of_tables_mut())
    else {
        return Ok(doc.to_string());
    };

    for table in nodes.iter_mut() {
        let id = table.get("id").and_then(|v| v.as_str()).map(str::to_string);
        if let Some(id) = id
            && let Some((_, path)) = updates.iter().find(|(uid, _)| uid == &id)
        {
            table["path"] = value(path.clone());
        }
    }

    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acquire::config::{AcquireConfig, TaskConfig};

    fn node(id: &str, namespace: Option<&str>, name: Option<&str>) -> GenNode {
        GenNode {
            id: id.to_string(),
            namespace: namespace.map(str::to_string),
            name: name.map(str::to_string),
            path: None,
            mode: None,
        }
    }

    #[test]
    fn poll_config_round_trips() {
        let conn = GenConnection {
            endpoint: "opc.tcp://host:4840".to_string(),
            trust_server: true,
            ..Default::default()
        };
        let toml = generate_toml(
            &conn,
            vec![
                node("ns=2;s=Foo", Some("urn:example"), Some("Foo")),
                node("i=2258", None, None),
            ],
            GenMode::Poll,
            Duration::from_secs(1),
        )
        .expect("generate");

        let parsed = AcquireConfig::from_toml(&toml).expect("re-parse generated config");
        assert_eq!(
            parsed.connection.endpoint.as_deref(),
            Some("opc.tcp://host:4840")
        );
        assert!(parsed.connection.trust_server);
        assert_eq!(parsed.selection.nodes.len(), 2);
        // Nodes are written in a deterministic `(namespace, id)` order so an
        // unchanged selection always serializes identically; the base-namespace
        // node (empty URI) therefore sorts ahead of the `urn:example` node.
        assert_eq!(parsed.selection.nodes[0].id, "i=2258");
        assert!(parsed.selection.nodes[0].namespace.is_none());
        assert_eq!(parsed.selection.nodes[1].id, "ns=2;s=Foo");
        assert_eq!(
            parsed.selection.nodes[1].namespace.as_deref(),
            Some("urn:example")
        );
        assert_eq!(parsed.selection.nodes[1].name.as_deref(), Some("Foo"));
        assert_eq!(parsed.tasks.len(), 1);
        assert!(matches!(parsed.tasks[0], TaskConfig::Poll { .. }));
    }

    #[test]
    fn subscribe_config_round_trips() {
        let conn = GenConnection {
            endpoint: "opc.tcp://host:4840".to_string(),
            ..Default::default()
        };
        let toml = generate_toml(
            &conn,
            vec![node("ns=2;s=Bar", None, None)],
            GenMode::Subscribe,
            Duration::from_millis(500),
        )
        .expect("generate");

        let parsed = AcquireConfig::from_toml(&toml).expect("re-parse generated config");
        assert!(matches!(parsed.tasks[0], TaskConfig::Subscribe { .. }));
    }

    #[test]
    fn mixed_mode_config_emits_both_tasks() {
        use crate::acquire::config::NodeMode;

        let conn = GenConnection {
            endpoint: "opc.tcp://host:4840".to_string(),
            ..Default::default()
        };
        let mut poll_node = node("ns=2;s=Poll", None, Some("P"));
        poll_node.mode = Some(GenMode::Poll);
        let mut sub_node = node("ns=2;s=Sub", None, Some("S"));
        sub_node.mode = Some(GenMode::Subscribe);
        let auto_node = node("ns=2;s=Auto", None, Some("A"));

        let toml = generate_toml(
            &conn,
            vec![poll_node, sub_node, auto_node],
            GenMode::Subscribe,
            Duration::from_secs(2),
        )
        .expect("generate");

        let parsed = AcquireConfig::from_toml(&toml).expect("re-parse generated config");
        // Both poll and subscribe task blocks are present.
        assert!(
            parsed
                .tasks
                .iter()
                .any(|t| matches!(t, TaskConfig::Poll { .. }))
        );
        assert!(
            parsed
                .tasks
                .iter()
                .any(|t| matches!(t, TaskConfig::Subscribe { .. }))
        );
        // Per-node modes survive; the auto node has no explicit mode.
        let by_id = |id: &str| {
            parsed
                .selection
                .nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.mode)
        };
        assert_eq!(by_id("ns=2;s=Poll"), Some(NodeMode::Poll));
        assert_eq!(by_id("ns=2;s=Sub"), Some(NodeMode::Subscribe));
        assert_eq!(by_id("ns=2;s=Auto"), Some(NodeMode::Auto));
        // The default is recorded because an auto node relies on it.
        assert_eq!(parsed.acquire.default_mode, Some(NodeMode::Subscribe));
    }

    #[test]
    fn update_selection_preserves_rest() {
        let original = "\
# my acquire config
[connection]
endpoint = \"opc.tcp://host:4840\"
trust_server = true

[selection]
node_ids = [\"ns=2;s=Old\"]

[output]
format = \"tedge\"

[[task]]
mode = \"poll\"
interval = \"5s\"
";
        let updated = update_selection_in_toml(
            original,
            &[
                node("ns=2;s=A", Some("urn:example"), Some("A")),
                node("ns=2;s=B", None, None),
            ],
        )
        .expect("update");

        // Comment and other tables survive.
        assert!(updated.contains("# my acquire config"));
        assert!(updated.contains("trust_server = true"));
        assert!(updated.contains("interval = \"5s\""));
        // Legacy flat array is migrated away.
        assert!(!updated.contains("node_ids"));

        let parsed = AcquireConfig::from_toml(&updated).expect("re-parse updated config");
        assert_eq!(parsed.selection.nodes.len(), 2);
        // Deterministic `(namespace, id)` ordering: the base-namespace node
        // (empty URI) sorts ahead of the `urn:example` node.
        assert_eq!(parsed.selection.nodes[0].id, "ns=2;s=B");
        assert!(parsed.selection.nodes[0].namespace.is_none());
        assert_eq!(parsed.selection.nodes[1].id, "ns=2;s=A");
        assert_eq!(
            parsed.selection.nodes[1].namespace.as_deref(),
            Some("urn:example")
        );
        assert_eq!(parsed.selection.nodes[1].name.as_deref(), Some("A"));
        assert!(parsed.selection.node_ids.is_empty());
        assert!(matches!(parsed.tasks[0], TaskConfig::Poll { .. }));
    }

    #[test]
    fn update_selection_adds_table_when_missing() {
        let original = "\
[connection]
endpoint = \"opc.tcp://host:4840\"
trust_server = true

[[task]]
mode = \"scan\"
";
        let updated =
            update_selection_in_toml(original, &[node("i=2258", None, None)]).expect("update");
        let parsed = AcquireConfig::from_toml(&updated).expect("re-parse");
        assert_eq!(parsed.selection.nodes.len(), 1);
        assert_eq!(parsed.selection.nodes[0].id, "i=2258");
    }

    #[test]
    fn update_node_paths_caches_browse_path() {
        let original = "\
# keep me
[connection]
endpoint = \"opc.tcp://host:4840\"

[[selection.node]]
id = \"ns=4;i=1276\"
namespace = \"urn:boiler\"
name = \"ControlOut\"

[[selection.node]]
id = \"ns=4;i=1244\"
name = \"Output\"

[output]
format = \"ndjson\"

[[task]]
mode = \"poll\"
interval = \"1s\"
";
        let updated = update_node_paths_in_toml(
            original,
            &[
                (
                    "ns=4;i=1276".to_string(),
                    "Boiler #1/LX001/ControlOut".to_string(),
                ),
                (
                    "ns=4;i=1244".to_string(),
                    "Boiler #1/CC001/Output".to_string(),
                ),
            ],
        )
        .expect("update paths");

        assert!(updated.contains("# keep me"));
        assert!(updated.contains("path = \"Boiler #1/LX001/ControlOut\""));
        assert!(updated.contains("path = \"Boiler #1/CC001/Output\""));

        let parsed = AcquireConfig::from_toml(&updated).expect("re-parse");
        assert_eq!(
            parsed.selection.nodes[0].path.as_deref(),
            Some("Boiler #1/LX001/ControlOut")
        );
        assert_eq!(
            parsed.selection.nodes[1].path.as_deref(),
            Some("Boiler #1/CC001/Output")
        );
    }

    #[test]
    fn update_node_paths_noop_without_updates() {
        let original = "[connection]\nendpoint = \"opc.tcp://host:4840\"\n";
        let updated = update_node_paths_in_toml(original, &[]).expect("noop");
        assert_eq!(updated, original);
    }
}
