# Component: CLI (`src/cli/`, `src/main.rs`)

> The command surface and per-command orchestration. Evidence: **[Confirmed]** unless noted.

## Purpose
Define every subcommand, flag, default, and value enum (clap derive), and orchestrate each command's
run sequence. Shared argument groups (`ConnectionArgs`, `TraversalArgs`, `FilterArgs`, `OutputArgs`)
are reused across subcommands so flags behave consistently.

## Top-level (`src/cli/mod.rs`)

```
Cli { verbose: u8 (-v..), quiet: bool (-q), profile: Option<String> (--profile NAME|PATH), command }
Command = Connect | Browse | Search | Sample | Acquire
```

### `ConnectionArgs`
| Field | Flag | Default | Notes |
| --- | --- | --- | --- |
| `endpoint` | positional `ENDPOINT` | — | must start with `opc.tcp://` |
| `user` | `--user` | — | |
| `password` | `--password` | — | `env:VAR` or secure prompt; requires `--user`; never logged |
| `cert` | `--cert FILE` | — | client cert (PEM/DER) |
| `key` | `--key FILE` | — | requires `--cert` |
| `security_policy` | `--security-policy` | `None` | all six async-opcua policies incl. deprecated `Basic128Rsa15`/`Basic256`; aliases: tedge-dot names, policy URIs |
| `security_mode` | `--security-mode` | — | `Option`; `effective_security()` → SignAndEncrypt for a secured policy, else None; rejects inconsistent pairs |
| `allow_deprecated_security` | `--allow-deprecated-security` | false | gate for the SHA-1 policies, checked in `Connection::connect` |
| `trust_server` | `--trust-server` | false | INSECURE; lab only |
| `pki_dir` | `--pki-dir DIR` | — | own cert/key/trusted |
| `timeout` | `--timeout` | `10s` | humantime |
| `retry` | `--retry` | `3` | transient retries |
| `cache` | `--cache DIR` | — | namespace-table cache |
| `tedge_dot_config` | `--tedge-dot-config FILE` | — | fill unset fields from a tedge-dot OPC UA connector config (`src/tedge_dot/connection.rs`, applied in `main.rs` after `--profile`) |
| `device` | `--device NAME` | — | device in that config; optional with one device |
| `extras` | — (`#[arg(skip)]`) | — | `ConnectionExtras`: literal password, application name/URI, borrowed application certificate, device `type` |

### `TraversalArgs` (browse/search/sample)
| Field | Flag | Default |
| --- | --- | --- |
| `depth` | `--depth N\|unlimited` | `3` (root=0; `-1`/`unlimited` = no limit) |
| `root` | `--root NODEID` | `i=85` (Objects) |
| `strategy` | `--strategy bfs\|dfs` | `bfs` |
| `workers` | `--workers N` | `8` |
| `page_size` | `--page-size N` | `200` |
| `max_children` | `--max-children N` | none |

### `FilterArgs` (browse/search/sample)
`include`/`include_regex`/`exclude`/`exclude_regex` (repeatable), `include_ns` (URIs), `ns_uri`,
`ns_index`, `node_class` (`--type`, csv), `datatype` (csv: builtin name/NodeId/URI), `readable`,
`writable`, `historizing`, `subscribable`.

### `OutputArgs`
`format` (`--format tree|table|json`, default `tree`), `output` (`--output json|ndjson|csv|tedge-dot`; `tedge-dot` is `browse`-only),
`file` (`--file`), `preview_values`, `tui`, `watch` (DURATION).

### Helpers
- `parse_duration` (humantime), `parse_depth` (`unlimited`/`-1` → `Unlimited`).
- `load_namespaces(session, connection, endpoint)` — cache-aware namespace read.
- Enums: `Depth { Limited(u32), Unlimited }` (`allows_children_at`), `Strategy`, `Format`,
  `Encoding`, `NodeClassArg`, `SecurityPolicyArg`, `SecurityModeArg`.

## Commands

### `connect` (`src/cli/connect.rs`)
`ConnectArgs { connection }`. Connect → read `ServerInfo` → read namespace table → print metadata,
namespaces, endpoints (+ security) → disconnect.

### `browse` (`src/cli/browse.rs`) — primary
`BrowseArgs { connection, traversal, filters, output, validate_subscription, config, library_type, library }`.
`--output tedge-dot` writes/merges a point library via `tedge_dot::library::write`; `--library`
(default `point-library.toml`) is the TUI's library, whose points are pre-marked when it exists.
Optionally pre-seed from `--config` (acquire.toml) → connect → load namespaces (cacheable) →
compile filters → if `--tui` launch explorer (early return) → else watch-loop: run `Engine`, stream
`NodeRecord`s, optionally validate subscriptions, render/export; repeat on `--watch`.

### `search` (`src/cli/search.rs`)
`SearchArgs { connection, query, traversal, filters, output, limit (50) }`. Require query → scan →
fuzzy score on name/path/description → keep top-N → render/export.

### `sample` (`src/cli/sample.rs`)
`SampleArgs { connection, traversal, filters, duration (10s) }`. Force `subscribable=true` → scan →
collect `(path, NodeId)` → `subscription::run_sample` prints live `path = value` for duration.

### `acquire` (`src/cli/acquire.rs`)
`AcquireArgs { config, config_dir, duration, poll_only }`. Load one file or a sorted directory of
configs → apply `--poll-only` override (can only turn on) → `acquire::run(...)` per config. See
[acquire.md](acquire.md).

## Behavioral notes
- `main.rs` merges profile, installs signal handler, dispatches, maps errors→exit codes, and exits
  `4` if cancelled after a clean return.
- `--quiet` conflicts with `--verbose`. `--password` requires `--user`; `--key` requires `--cert`.

## Extension points
- New subcommand → add a `Command` variant + `src/cli/<name>.rs` with `run()` + wire in `main.rs`.
- New shared flag → add to the relevant `*Args` group so all commands inherit it.
