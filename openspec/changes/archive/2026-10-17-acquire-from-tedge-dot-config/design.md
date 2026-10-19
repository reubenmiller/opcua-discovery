# Design

## Context

`acquire::run(config, duration, config_path, cancel)` builds its connection from
`config.connection` and writes reconstructed browse paths back to `config_path`. Explicit nodes are
`NodeSelector { id, namespace (URI), name, path, mode }`; `selection::remap_node_id` already
replaces a node id's index with the server's index for `namespace` and fails when the URI is
missing. Samples are keyed by `meta.browse_path`, else the node name. tedge-dot's library loader
(`sdk/src/library.rs`: `search_path`, `locate`, `is_path_reference`, `merge_point`) defines how
`points_from` is resolved.

## Decisions

**Points become `NodeSelector`s.** `src/tedge_dot/points.rs` resolves the device's points and
emits, per readable point, a selector whose `id` is the node id as written, `namespace` the URI
from its table (none for ns 0 or without a table), `name` the point's `name` (else its id), `mode`
`poll` for `subscribe = false` else `auto`, and a new `key` (the point id). The existing remap does
index → URI → server index, which is exactly the PR's safety rule; nothing new is needed there.

**Output key override.** `NodeSelector.key` (`#[serde(skip)]`, so acquire files cannot set it)
flows into `SampleMeta.key`; `output_key` prefers it over the browse path.

**Library resolution mirrors tedge-dot**, without the management-command trust checks (we only
read files the user names). Inline points replace library points with the same id as a whole
(tedge-dot deep-merges `meta`/`transform`/`report`, none of which acquire uses).

**`run` takes its connection.** `acquire::run(config, connection_args, duration, write_back,
cancel)`: the file path flows in as `write_back: Option<&Path>` and is `None` for tedge-dot runs;
file-config callers pass `config.connection_args()?` and their path as before. The tedge-dot path
builds `ConnectionArgs` from `ConnectionConfig::default().to_args()` plus `tedge_dot_config`/
`device`, then `tedge_dot::connection::apply`.

**Extras file** is checked at the TOML table level (`acquire`, `output`, `task` only) and then
deserialised into `AcquireConfig` without the selection validation.

**Per-point `poll_interval`** is not honoured (acquire runs one poll task); documented. tedge-dot's
default 2s is used when neither the device nor the connector sets one.
