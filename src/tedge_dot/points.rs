//! The points a tedge-dot connector config defines for one device, as acquire
//! node selectors: `points_from` libraries (found like tedge-dot finds them,
//! `sdk/src/library.rs`) followed by inline `[[device.point]]` entries, a later
//! point with the same `id` replacing an earlier one.
//!
//! Each node id keeps the index it was written with and carries the URI its
//! namespace table gives that index, so acquire's remap reads the node at the
//! server's current index for that URI (and fails when the server lacks it).

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use opcua::types::{Identifier, NodeId};
use tracing::warn;

use crate::acquire::config::{NodeMode, NodeSelector};
use crate::error::{Result, ScanError};

/// Where tedge-dot looks for libraries when nothing overrides it.
const SITE_LIBRARY_DIR: &str = "/etc/tedge/plugins/ot/points.d";
const PACKAGED_LIBRARY_DIR: &str = "/usr/share/tedge-dot/points.d";
const LIBRARY_PATH_ENV: &str = "TEDGE_DOT_POINT_LIBRARY_PATH";
/// tedge-dot's `poll_interval` when neither the device nor the connector sets one.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const OPCUA_NS: &str = "http://opcfoundation.org/UA/";

/// A device's readable points and its intervals.
#[derive(Debug)]
pub struct DevicePoints {
    pub selectors: Vec<NodeSelector>,
    /// `(point id, reason)` of the points acquire cannot read.
    pub skipped: Vec<(String, String)>,
    pub poll_interval: Duration,
    pub sampling_interval: Option<Duration>,
}

/// One resolved point and the namespace table its node id was written against.
struct Point {
    table: toml::Table,
    namespaces: Option<Vec<String>>,
}

/// Read `path` and resolve the points of `device` (or of the only device).
pub fn load(path: &Path, device: Option<&str>) -> Result<DevicePoints> {
    let config_err = |reason: String| ScanError::Config {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|e| config_err(e.to_string()))?;
    let doc: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| config_err(e.to_string()))?;
    let base_dir = path
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        })
        .and_then(|p| std::fs::canonicalize(p).ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let env = std::env::var(LIBRARY_PATH_ENV).ok();
    let points = resolve(&doc, device, &base_dir, env.as_deref()).map_err(config_err)?;
    for (id, reason) in &points.skipped {
        warn!(point = %id, "not acquired: {reason}");
    }
    Ok(points)
}

/// Resolve the device's points from a parsed config.
fn resolve(
    doc: &toml::Table,
    device: Option<&str>,
    base_dir: &Path,
    env_path: Option<&str>,
) -> std::result::Result<DevicePoints, String> {
    let table = super::connection::select_device(doc, device)?;
    let name = table
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or("?")
        .to_string();
    let device_err = |e: String| format!("device '{name}': {e}");
    let device_namespaces = table
        .get("protocol_address")
        .and_then(|a| a.get("namespaces"))
        .map(|v| namespace_table(v, "protocol_address namespaces"))
        .transpose()
        .map_err(device_err)?;

    // Libraries in order, then inline points; a later id replaces an earlier one.
    let mut points: Vec<Point> = Vec::new();
    let search = search_path(doc, base_dir, env_path)?;
    for reference in table
        .get("points_from")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
    {
        let file = locate(reference, base_dir, &search)?;
        let (lib_points, lib_namespaces) = read_library(&file)?;
        for p in lib_points {
            merge(
                &mut points,
                Point {
                    table: p,
                    namespaces: lib_namespaces.clone().or_else(|| device_namespaces.clone()),
                },
            );
        }
    }
    for p in table
        .get("point")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
    {
        merge(
            &mut points,
            Point {
                table: p.clone(),
                namespaces: device_namespaces.clone(),
            },
        );
    }

    let mut selectors = Vec::new();
    let mut skipped = Vec::new();
    for p in &points {
        let id = p
            .table
            .get("id")
            .and_then(toml::Value::as_str)
            .unwrap_or("?")
            .to_string();
        match selector(&id, p).map_err(device_err)? {
            Ok(Some(s)) => selectors.push(s),
            Ok(None) => {}
            Err(reason) => skipped.push((id, reason)),
        }
    }
    if selectors.is_empty() {
        return Err(device_err(
            "the device has no point acquire can read".to_string(),
        ));
    }

    let connector = doc.get("connector");
    let interval = |key: &str| -> std::result::Result<Option<Duration>, String> {
        table
            .get(key)
            .or_else(|| connector.and_then(|c| c.get(key)))
            .and_then(toml::Value::as_str)
            .map(|s| humantime::parse_duration(s).map_err(|e| format!("{key} '{s}': {e}")))
            .transpose()
    };
    Ok(DevicePoints {
        selectors,
        skipped,
        poll_interval: interval("poll_interval")?.unwrap_or(DEFAULT_POLL_INTERVAL),
        sampling_interval: interval("sampling_interval")?,
    })
}

/// Add `point`, or replace the point with the same id.
fn merge(points: &mut Vec<Point>, point: Point) {
    let id = point.table.get("id").and_then(toml::Value::as_str);
    match points
        .iter_mut()
        .find(|p| id.is_some() && p.table.get("id").and_then(toml::Value::as_str) == id)
    {
        Some(existing) => *existing = point,
        None => points.push(point),
    }
}

/// The selector for one point: `Ok(Some)` to acquire, `Ok(None)` for a
/// disabled point, `Err(reason)` to skip and report. The outer error is a
/// configuration error that fails the run.
fn selector(
    id: &str,
    p: &Point,
) -> std::result::Result<std::result::Result<Option<NodeSelector>, String>, String> {
    let t = &p.table;
    if t.get("enabled").and_then(toml::Value::as_bool) == Some(false) {
        return Ok(Ok(None));
    }
    match t
        .get("access")
        .and_then(toml::Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("call_method" | "call-method") => return Ok(Err("method point".into())),
        Some("write") => return Ok(Err("write-only point".into())),
        _ => {}
    }
    let Some(address) = t.get("address").and_then(toml::Value::as_table) else {
        return Ok(Err("no address".into()));
    };
    if address.contains_key("method_id") {
        return Ok(Err("method point".into()));
    }
    for key in ["field", "index"] {
        if address.contains_key(key) {
            return Ok(Err(format!(
                "`{key}` points select part of a value, which acquire cannot"
            )));
        }
    }
    let node = address_node(address).map_err(|e| format!("point '{id}': {e}"))?;
    let namespace = match (&p.namespaces, node.namespace) {
        (_, 0) | (None, _) => None,
        (Some(table), ns) => Some(
            table
                .get(ns as usize)
                .ok_or_else(|| {
                    format!(
                        "point '{id}': namespace index {ns} is not in the namespace table ({} entries)",
                        table.len()
                    )
                })?
                .clone(),
        ),
    };
    let mode = match t.get("subscribe").and_then(toml::Value::as_bool) {
        Some(false) => NodeMode::Poll,
        _ => NodeMode::Auto,
    };
    Ok(Ok(Some(NodeSelector {
        id: node.to_string(),
        namespace,
        name: Some(
            t.get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or(id)
                .to_string(),
        ),
        path: None,
        mode,
        key: Some(id.to_string()),
    })))
}

/// `address.node_id`, or `namespace` + `identifier`.
fn address_node(address: &toml::Table) -> std::result::Result<NodeId, String> {
    if let Some(text) = address.get("node_id").and_then(toml::Value::as_str) {
        return NodeId::from_str(text).map_err(|_| format!("invalid node_id '{text}'"));
    }
    let ns = address
        .get("namespace")
        .and_then(toml::Value::as_integer)
        .and_then(|n| u16::try_from(n).ok())
        .ok_or("address needs node_id, or namespace and identifier")?;
    let identifier = match address.get("identifier") {
        Some(toml::Value::String(s)) => Identifier::String(s.as_str().into()),
        Some(toml::Value::Integer(n)) => Identifier::Numeric(
            u32::try_from(*n).map_err(|_| format!("identifier {n} is out of range"))?,
        ),
        _ => return Err("address needs node_id, or namespace and identifier".into()),
    };
    Ok(NodeId::new(ns, identifier))
}

/// A `namespaces` array: strings, entry 0 the OPC UA namespace.
fn namespace_table(value: &toml::Value, what: &str) -> std::result::Result<Vec<String>, String> {
    let table: Vec<String> = value
        .as_array()
        .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
        .ok_or_else(|| format!("{what} must be an array of strings"))?;
    if table.first().map(String::as_str) != Some(OPCUA_NS) {
        return Err(format!("{what} entry 0 must be {OPCUA_NS}"));
    }
    Ok(table)
}

/// The library directories, in tedge-dot's order of precedence.
fn search_path(
    doc: &toml::Table,
    base_dir: &Path,
    env_path: Option<&str>,
) -> std::result::Result<Vec<PathBuf>, String> {
    if let Some(configured) = doc
        .get("connector")
        .and_then(|c| c.get("point_library_path"))
    {
        let dirs = configured
            .as_array()
            .ok_or("[connector] point_library_path must be an array of directories")?;
        return dirs
            .iter()
            .map(|d| {
                d.as_str()
                    .map(|d| base_dir.join(d))
                    .ok_or_else(|| "[connector] point_library_path entries must be strings".into())
            })
            .collect();
    }
    if let Some(env) = env_path {
        let dirs: Vec<PathBuf> = env
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|d| base_dir.join(d))
            .collect();
        if !dirs.is_empty() {
            return Ok(dirs);
        }
    }
    Ok(vec![
        PathBuf::from(SITE_LIBRARY_DIR),
        PathBuf::from(PACKAGED_LIBRARY_DIR),
    ])
}

/// The file a `points_from` entry names: a path, or `<dir>/opcua/<name>.toml`.
fn locate(
    reference: &str,
    base_dir: &Path,
    search: &[PathBuf],
) -> std::result::Result<PathBuf, String> {
    if reference.ends_with(".toml") || reference.contains('/') {
        let path = base_dir.join(reference);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "point library '{reference}' not found at {}",
                path.display()
            ))
        };
    }
    let mut tried = Vec::new();
    for dir in search {
        let candidate = dir.join("opcua").join(format!("{reference}.toml"));
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }
    Err(format!(
        "unknown point library '{reference}' (looked for {})",
        tried.join(", ")
    ))
}

/// A library's points and its namespace table.
#[allow(clippy::type_complexity)]
fn read_library(
    path: &Path,
) -> std::result::Result<(Vec<toml::Table>, Option<Vec<String>>), String> {
    let where_ = path.display();
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("point library '{where_}': {e}"))?;
    let doc: toml::Table = text
        .parse()
        .map_err(|e| format!("point library '{where_}': {e}"))?;
    let library = doc.get("library");
    match library
        .and_then(|l| l.get("protocol"))
        .and_then(toml::Value::as_str)
    {
        Some("opcua") => {}
        Some(other) => {
            return Err(format!(
                "point library '{where_}' is for protocol '{other}', not 'opcua'"
            ));
        }
        None => {
            return Err(format!(
                "point library '{where_}' has no [library] protocol"
            ));
        }
    }
    let namespaces = library
        .and_then(|l| l.get("namespaces"))
        .map(|v| namespace_table(v, &format!("point library '{where_}': namespaces")))
        .transpose()?;
    let points = doc
        .get("point")
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(toml::Value::as_table)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok((points, namespaces))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: &str = r#"
[library]
protocol = "opcua"
namespaces = ["http://opcfoundation.org/UA/", "urn:server", "urn:acme"]

[[point]]
id = "flow"
datatype = "float64"
address = { node_id = "ns=2;s=Flow" }

[[point]]
id = "level"
datatype = "float64"
address = { node_id = "ns=2;s=Level" }

[[point]]
id = "start"
access = "call_method"
address = { object_id = "ns=2;s=Pump", method_id = "ns=2;s=Start" }
"#;

    /// A config dir with `opcua.toml` (device text appended) and `lib/opcua/pump.toml`.
    fn setup(device: &str) -> (tempfile::TempDir, toml::Table) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lib/opcua")).unwrap();
        std::fs::write(dir.path().join("lib/opcua/pump.toml"), LIBRARY).unwrap();
        let text = format!(
            "[connector]\nprotocol = \"opcua\"\npoint_library_path = [\"lib\"]\npoll_interval = \"3s\"\n\n{device}"
        );
        (dir, text.parse().unwrap())
    }

    fn resolve_in(
        dir: &tempfile::TempDir,
        doc: &toml::Table,
    ) -> std::result::Result<DevicePoints, String> {
        resolve(doc, None, dir.path(), None)
    }

    fn by_key<'a>(points: &'a DevicePoints, key: &str) -> &'a NodeSelector {
        points
            .selectors
            .iter()
            .find(|s| s.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("no point {key}"))
    }

    #[test]
    fn library_points_carry_their_namespace_uri() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840" }
points_from = ["pump"]
"#,
        );
        let p = resolve_in(&dir, &doc).unwrap();
        let flow = by_key(&p, "flow");
        assert_eq!(flow.id, "ns=2;s=Flow");
        assert_eq!(flow.namespace.as_deref(), Some("urn:acme"));
        assert_eq!(flow.name.as_deref(), Some("flow"));
        assert_eq!(flow.mode, NodeMode::Auto);
        assert_eq!(
            p.skipped,
            [("start".to_string(), "method point".to_string())]
        );
        assert_eq!(p.poll_interval, Duration::from_secs(3));
    }

    #[test]
    fn inline_point_replaces_library_point() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
poll_interval = "5s"
protocol_address = { endpoint = "opc.tcp://x:4840", namespaces = ["http://opcfoundation.org/UA/", "urn:other"] }
points_from = ["pump"]

  [[device.point]]
  id = "level"
  name = "Tank level"
  datatype = "float64"
  subscribe = false
  address = { node_id = "ns=1;i=7" }
"#,
        );
        let p = resolve_in(&dir, &doc).unwrap();
        assert_eq!(p.selectors.len(), 2);
        let level = by_key(&p, "level");
        assert_eq!(level.id, "ns=1;i=7");
        assert_eq!(level.namespace.as_deref(), Some("urn:other"));
        assert_eq!(level.name.as_deref(), Some("Tank level"));
        assert_eq!(level.mode, NodeMode::Poll);
        assert_eq!(p.poll_interval, Duration::from_secs(5));
    }

    #[test]
    fn library_without_table_uses_the_devices() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840", namespaces = ["http://opcfoundation.org/UA/", "urn:a", "urn:b"] }
points_from = ["plain"]
"#,
        );
        std::fs::write(
            dir.path().join("lib/opcua/plain.toml"),
            "[library]\nprotocol = \"opcua\"\n[[point]]\nid = \"t\"\ndatatype = \"float64\"\naddress = { namespace = 2, identifier = 5 }\n",
        )
        .unwrap();
        let p = resolve_in(&dir, &doc).unwrap();
        let t = by_key(&p, "t");
        assert_eq!(t.id, "ns=2;i=5");
        assert_eq!(t.namespace.as_deref(), Some("urn:b"));
    }

    #[test]
    fn no_table_keeps_the_literal_index_and_ns0_is_never_mapped() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840" }
  [[device.point]]
  id = "a"
  datatype = "float64"
  address = { node_id = "ns=3;i=1" }
  [[device.point]]
  id = "time"
  datatype = "string"
  address = { node_id = "i=2258" }
"#,
        );
        let p = resolve_in(&dir, &doc).unwrap();
        assert_eq!(by_key(&p, "a").namespace, None);
        assert_eq!(by_key(&p, "time").namespace, None);
        assert_eq!(by_key(&p, "time").id, "i=2258");
    }

    #[test]
    fn skips_and_disabled() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840" }
  [[device.point]]
  id = "ok"
  datatype = "float64"
  address = { node_id = "ns=2;i=1" }
  [[device.point]]
  id = "off"
  enabled = false
  datatype = "float64"
  address = { node_id = "ns=2;i=2" }
  [[device.point]]
  id = "setpoint"
  access = "write"
  datatype = "float64"
  address = { node_id = "ns=2;i=3" }
  [[device.point]]
  id = "current"
  datatype = "float32"
  address = { node_id = "ns=2;i=4", field = "Motor.Current" }
"#,
        );
        let p = resolve_in(&dir, &doc).unwrap();
        assert_eq!(p.selectors.len(), 1);
        let skipped: Vec<&str> = p.skipped.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(skipped, ["setpoint", "current"]);
    }

    #[test]
    fn nothing_readable_is_an_error() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840" }
  [[device.point]]
  id = "m"
  access = "call_method"
  address = { object_id = "i=85", method_id = "ns=2;i=9" }
"#,
        );
        let err = resolve_in(&dir, &doc).unwrap_err();
        assert!(err.contains("no point acquire can read"), "{err}");
    }

    #[test]
    fn index_outside_the_table_is_an_error() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840", namespaces = ["http://opcfoundation.org/UA/"] }
  [[device.point]]
  id = "x"
  datatype = "float64"
  address = { node_id = "ns=5;i=1" }
"#,
        );
        let err = resolve_in(&dir, &doc).unwrap_err();
        assert!(
            err.contains("point 'x'") && err.contains("index 5"),
            "{err}"
        );
    }

    #[test]
    fn unknown_library_lists_the_tried_paths() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840" }
points_from = ["nonexistent"]
"#,
        );
        let err = resolve_in(&dir, &doc).unwrap_err();
        assert!(
            err.contains("nonexistent") && err.contains("lib/opcua/nonexistent.toml"),
            "{err}"
        );
    }

    #[test]
    fn path_reference_is_relative_to_the_config() {
        let (dir, doc) = setup(
            r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840" }
points_from = ["lib/opcua/pump.toml"]
"#,
        );
        assert_eq!(resolve_in(&dir, &doc).unwrap().selectors.len(), 2);
    }

    #[test]
    fn search_path_precedence() {
        let base = Path::new("/cfg");
        let configured: toml::Table = "[connector]\npoint_library_path = [\"a\", \"/b\"]"
            .parse()
            .unwrap();
        assert_eq!(
            search_path(&configured, base, Some("/env")).unwrap(),
            [PathBuf::from("/cfg/a"), PathBuf::from("/b")]
        );
        let plain = toml::Table::new();
        assert_eq!(
            search_path(&plain, base, Some("/x:/y")).unwrap(),
            [PathBuf::from("/x"), PathBuf::from("/y")]
        );
        assert_eq!(
            search_path(&plain, base, None).unwrap(),
            [
                PathBuf::from(SITE_LIBRARY_DIR),
                PathBuf::from(PACKAGED_LIBRARY_DIR)
            ]
        );
    }

    #[test]
    fn default_poll_interval_is_tedge_dots() {
        let dir = tempfile::tempdir().unwrap();
        let doc: toml::Table = r#"[[device]]
name = "pump"
protocol_address = { endpoint = "opc.tcp://x:4840" }
  [[device.point]]
  id = "a"
  datatype = "float64"
  address = { node_id = "ns=2;i=1" }
"#
        .parse()
        .unwrap();
        let p = resolve(&doc, None, dir.path(), None).unwrap();
        assert_eq!(p.poll_interval, DEFAULT_POLL_INTERVAL);
        assert_eq!(p.sampling_interval, None);
    }
}
