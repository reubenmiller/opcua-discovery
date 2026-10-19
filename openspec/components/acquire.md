# Component: Acquire (`src/acquire/`)

> Config-driven data acquisition: schema, node selection, sinks, orchestration, and config
> generation. Evidence: **[Confirmed]** unless noted.

## Overview
`acquire` connects once, resolves a node selection from a TOML config, partitions nodes into poll vs
subscribe by **effective mode**, and runs scan/poll/subscribe/history tasks concurrently into one
`Sink`. Reconstructed browse paths are cached back into the config file.

---

## Config schema (`config.rs`) — `AcquireConfig` (`#[serde(deny_unknown_fields)]`)

Tables: `[connection]`, `[acquire]`, `[selection]`, `[output]`, and one or more `[[task]]`
(`#[serde(rename = "task")]`, **≥1 required**).

### `[connection]` — `ConnectionConfig`
`endpoint`, `name` (→ `meta.source.name`), `user`, `password` (`env:VAR`), `cert`, `key`,
`security_policy`, `security_mode`, `trust_server` (def false), `pki_dir`, `timeout` (humantime),
`retry`, `cache`. Methods `to_args()`, `apply_to(&mut ConnectionArgs)` (CLI precedence).

### `[acquire]` — `AcquireOptions`
- `default_mode: Option<NodeMode>` — applied to nodes without a mode; when unset, inferred from tasks.
- `poll_only: bool` (def false) — force all nodes to poll.
- **`NodeMode`** (`lowercase`): `auto` (default) | `subscribe` | `poll`.

### `[selection]` — `SelectionConfig`
- `node_ids: Vec<String>` — legacy bare ids (index not remapped).
- `nodes: Vec<NodeSelector>` (`#[serde(rename = "node")]`) — rich entries.
- `root: Option<String>` (def `i=85`), `depth: Option<String>` (number/`unlimited`, def 3).
- Filters: `include`, `include_regex`, `exclude`, `exclude_regex`, `include_ns`, `node_class`,
  `datatype`.
- Methods: `depth() -> Depth`, `to_filter_args() -> FilterArgs`.

#### `[[selection.node]]` — `NodeSelector`
`id` (e.g. `ns=4;i=1274`), `namespace` (URI; remaps `id`'s index to the server's current index for
that URI), `name` (measurement name), `path` (cached browse path; reconstructed + cached on first run
if absent), `mode: NodeMode` (def `auto`).

### `[output]` — `OutputConfig`
- `format: OutputFormat` (`lowercase`): `tedge` (default) | `json` | `ndjson` | `csv`.
- `file: Option<PathBuf>` (else stdout), `topic: Option<String>` (MQTT topic **prefix** hint for
  downstream `tedge mqtt pub` — no MQTT client here **[Likely]**),
  `metadata: MetadataMode` (`nested` default | `off`).

### `[[task]]` — `TaskConfig` (`#[serde(tag = "mode", rename_all = "lowercase")]`)
| `mode` | Variant fields |
| --- | --- |
| `scan` | `strategy` (bfs/dfs), `workers?`, `page_size?`, `max_children?`, `preview_values` (def false) |
| `poll` | `interval: Duration` (**required**, humantime) |
| `subscribe` | `publishing_interval?`, `sampling_interval?`, `queue_size?` |
| `history` | `start: String` (RFC3339, **required**), `end?` (def now), `max_values?` |

### Derived logic on `AcquireConfig`
- `effective_default_mode()` — explicit if set; else inferred (poll-only tasks → poll; subscribe-only
  → subscribe; both/neither → auto).
- `poll_interval()` — first poll task's interval, else `DEFAULT_POLL_INTERVAL = 1s`.
- `subscribe_params()` — first subscribe task's params.
- `from_file` / `from_toml` — parse + validate (≥1 task; node selection required for non-scan tasks).

---

## Orchestration (`mod.rs`)
`run(config, duration: Option<Duration>, config_path: Option<&Path>, cancel) -> Result<()>`:
1. `Connection::connect` (once) → shared `Arc<Session>`.
2. `load_namespaces` (cache-aware) for URI remapping.
3. Build `Sink` from `[output]`.
4. If poll/subscribe/history tasks exist → `selection::resolve` → `Resolved { nodes, path_updates }`;
   optionally `persist_node_paths` (write reconstructed paths back via `toml_edit`).
5. `partition_nodes(nodes, effective_default_mode, poll_only) -> (poll_nodes, subscribe_nodes)` using
   `effective_subscribe(node, default, poll_only)`:
   - `poll_only` → always poll.
   - explicit node mode wins; `auto` → subscribe iff `subscribable && server supports monitoring`,
     else poll.
6. Spawn a `JoinSet`: `run_scan` (scan tasks), `run_history` (history tasks), one `run_poll` loop,
   one `run_subscription` — all → `Sink`.
7. If `duration` set, a timer sets `cancel` to bound continuous tasks.
8. Await all → `Sink::finish()` → disconnect.

- `run_poll` — ticker → batch `read_readings(nodes)` → emit `Sample`s.
- `run_history` — `history::read_raw` per node over the time range → emit `Sample`s.
- Unit tests cover poll_only override, explicit-mode-wins, auto-follows-default, auto-uses-
  subscribability, and partition routing.

---

## Selection (`selection.rs`)
- **`SelectedNode`** `{ name, path, node_id, mode, subscribable, meta: SampleMeta }`.
- **`Resolved`** `{ nodes, path_updates: Vec<(config_id, reconstructed_path)> }`.
- `resolve(session, config, namespaces, endpoint, cancel)` stages:
  1. **Explicit** (`resolve_explicit`): parse `id` + optional `namespace` (`remap_node_id`), read
     attributes, skip unreadable, resolve path (cached or `reconstruct_browse_path`), build
     `SampleMeta`; dedupe by `NodeId`.
  2. **Filtered** (`resolve_filtered`, if filters present): run `Engine` with `subscribable=true`;
     discovered nodes default to `auto`.
  3. **Enrich** (`enrich_metadata`): fold `EURange`/`EngineeringUnits` properties into parents and
     drop them as standalone nodes.
  4. **Stamp** `source_endpoint` / `source_name` on every node.

---

## Sink (`sink.rs`)
- **`Sample`** `{ name, node_id, value: serde_json::Value, time, status, meta: Arc<SampleMeta> }`.
- **`SampleMeta`** `{ data_type, engineering_units, browse_name, display_name, browse_path,
  namespace, eu_range: Option<(f64,f64)>, source_endpoint, source_name }` + `descriptive_json()`,
  `output_key(fallback)`.
- **`Sink`** `{ format, topic, metadata, state: Mutex<State> }`:
  - `new(&OutputConfig)`, `emit_sample`, `emit_record`, `finish`.
  - `tedge`/`ndjson` write immediately; `json`/`csv` buffer then flush on `finish`.
- Output shapes:
  - `tedge`: `{ "time", "<key>": <value>, "meta": {...} }` (key = browse path or name; `meta` omitted
    when `MetadataMode::Off`).
  - `ndjson`/`json`: stable record `{ time, key, value, dataType, nodeId, units, status, meta }`
    (off-mode trims to `time, key, value, nodeId, status`).
  - `csv`: union-of-keys header, metadata flattened, buffered.

---

## Config generation (`generate.rs`) — used by the TUI
- `GenMode` (`Poll`/`Subscribe`), `GenConnection` (endpoint, trust_server, policy/mode, user [password
  not written — comment hint], pki_dir), `GenNode` (id, namespace, name, path, mode?).
- `generate_toml(conn, nodes, default_mode, interval)` — stable sort by `(namespace, id)`; emit poll
  and/or subscribe task blocks (or one default-mode task if selection empty); emit `[acquire]
  default_mode` only if any node is auto; prepend a "generated by TUI" header.
- `update_selection_in_toml(original, nodes)` — preserve formatting (`toml_edit`); drop legacy
  `node_ids`; replace `[[selection.node]]` entries.
- `update_node_paths_in_toml(original, updates)` — add/update `path` on matching node entries.

---

## Example (working) config
```toml
[connection]
endpoint = "opc.tcp://host:4840"
trust_server = true

[acquire]
default_mode = "auto"   # auto | subscribe | poll

[selection]
[[selection.node]]
id = "ns=3;i=1001"
name = "Temperature"
mode = "subscribe"

[output]
format = "tedge"

[[task]]
mode = "poll"
interval = "1s"
[[task]]
mode = "subscribe"
publishing_interval = "500ms"
```

## Acquire from a tedge-dot config (`src/cli/acquire.rs`, `src/tedge_dot/points.rs`)
`acquire --tedge-dot-config FILE [--device NAME] [--config extras.toml]`:
`tedge_dot::points::load` resolves the device's points (`points_from` libraries via tedge-dot's
search path, then inline points, later `id` replacing earlier) into `NodeSelector`s: `id` as
written, `namespace` = URI of its index in the applicable table (library, else device; none for
ns 0 / no table), `mode` poll for `subscribe = false` else auto, and `key` = point id
(`#[serde(skip)]`, carried into `SampleMeta.key`, which `output_key` prefers). The existing
`remap_node_id` then maps URI → server index and fails on a missing URI. The connection comes from
`tedge_dot::connection::apply`; `AcquireConfig::extras_from_file` accepts only `[acquire]`,
`[output]`, `[[task]]`; default tasks come from the device/connector `poll_interval` (2s) and
`sampling_interval`. `acquire::run` takes the `ConnectionArgs` and an optional write-back path,
which is `None` here, so the tedge-dot files are never modified.

## Extension points
- New task type → add a `TaskConfig` variant + a `run_*` task + spawn wiring.
- New sink format → add an `OutputFormat` variant + `emit_*`/`finish` branches.
- New metadata field → extend `SampleMeta` + `descriptive_json`.
