# opcua-scan — OpenSpec

> Reverse-engineered specification for the `opcua-scan` CLI (`opcua-discovery` repository).
> Written in the [OpenSpec](https://github.com/Fission-AI/OpenSpec) convention: capability-oriented
> requirements with `SHALL`/`WHEN`/`THEN` scenarios, so a new engineer or AI coding agent can
> understand, extend, or recreate the project.
>
> **Evidence labels** used throughout:
> - **[Confirmed]** — directly verified in source code.
> - **[Likely]** — strongly implied by usage, names, or tests but not exhaustively traced.
> - **[Assumption]** — inferred; needs manual validation. See [assumptions.md](assumptions.md).

---

## 1. Overview / Purpose

`opcua-scan` is a production-grade command-line tool for **scanning, discovering, and acquiring data
from an OPC UA server's address space**. It connects to an OPC UA endpoint, traverses the address
space concurrently and safely, reports nodes with rich metadata, and can stream live/historical data
to machine-readable sinks (including [thin-edge.io](https://thin-edge.io) measurement format).

- **Binary name:** `opcua-scan` **[Confirmed]** (`Cargo.toml` `[[bin]]`).
- **Crate name:** `opcua-scan`, version `0.1.0`, edition 2024, `rust-version = 1.88`. **[Confirmed]**
- **License:** MPL-2.0 **[Confirmed]** (`Cargo.toml`; note `README.md` also states MPL-2.0).
- **Language/runtime:** Rust, async on Tokio. **[Confirmed]**
- **Core dependency:** [`async-opcua`](https://github.com/freeopcua/async-opcua) `0.18` (client feature). **[Confirmed]**
- **Target scale:** address spaces of 10,000–500,000+ nodes; traversal is concurrent, paginated,
  cycle-safe, and streamed to keep memory low. **[Confirmed in README; design corroborated in code]**

### What it does (one paragraph)

The user invokes a subcommand (`connect`, `browse`, `search`, `sample`, `acquire`) against an OPC UA
endpoint. The tool establishes a secure session, reads the server's namespace table, and — for
discovery commands — runs a bounded-concurrency worker pool that browses hierarchical references,
batch-reads node attributes, applies path + attribute filters, and emits `NodeRecord`s. Records are
rendered to a human view (tree/table/json) or exported (json/ndjson/csv) to stdout or a file. The
`acquire` command is config-file driven and continuously polls and/or subscribes to selected
variables (and can read history), emitting samples to a sink in thin-edge.io or generic formats. An
optional interactive TUI provides lazy, on-demand exploration and can generate an `acquire` config.

---

## 2. Goals and Non-Goals

### Goals **[Confirmed from code/README]**
- G1. Efficiently discover huge OPC UA address spaces without exhausting memory or overloading the
  server (bounded workers, pagination, continuation points, retry/backoff, cycle protection).
- G2. Provide expressive filtering (glob/regex path, node class, datatype, namespace, access level,
  subscribable heuristic) applied **during** traversal so excluded branches are not browsed.
- G3. Emit both human-readable and machine-readable output; never pollute machine output on stdout
  (all logs go to stderr).
- G4. Support secure-by-default connections (security policy/mode, PKI, user/password, X.509),
  with secrets never logged and server certs auto-trusted only via explicit `--trust-server`.
- G5. Acquire live and historical data with per-node `subscribe`/`poll`/`auto` routing, emitting
  thin-edge.io-compatible measurements.
- G6. Offer an interactive TUI for lazy exploration and config generation.
- G7. Be reproducible/operable: profiles for saved defaults, best-effort metadata cache, watch mode,
  stable exit codes, graceful Ctrl+C.

### Non-Goals **[Likely / Assumption]**
- NG1. Not a full offline mirror of the address space; the cache stores only the namespace table. **[Confirmed]** (`src/cache/mod.rs`).
- NG2. Not an OPC UA **server** (server feature is dev-only, used by integration tests). **[Confirmed]** (`Cargo.toml` dev-deps).
- NG3. Not a write/command tool — there is no node-writing or method-calling command surface. **[Likely]** (no write command found).
- NG4. No built-in MQTT client; thin-edge output is intended to be piped to `tedge mqtt pub`. **[Likely]** (`topic` field is a prefix hint; no MQTT dependency in `Cargo.toml`).
- NG5. Not a GUI/desktop app beyond the terminal TUI. **[Confirmed]**

---

## 3. System Architecture (summary)

See [architecture.md](architecture.md) for the full picture and data-flow diagram. In brief:

```
CLI (clap) ──> Profile merge ──> Connection (async-opcua session)
                                      │
                 ┌────────────────────┼─────────────────────────┐
                 │                    │                          │
            NamespaceTable        FilterSet                 (cache, optional)
                 │                    │
                 └──────> Scanner Engine (worker pool) ──> NodeRecord stream
                                      │                          │
                              ┌───────┴────────┐          ┌──────┴───────┐
                           Export (tree/      Subscription        TUI (lazy
                           table/json/        (validate/sample/   explorer +
                           ndjson/csv)        live)               acquire-gen)
```

The `acquire` command is a parallel orchestration path: it resolves a node selection, partitions
nodes into poll vs subscribe, and runs scan/poll/subscribe/history tasks concurrently into a `Sink`.

---

## 4. Capabilities & Requirements

Requirements are grouped by capability. Each uses `SHALL`, with at least one `WHEN/THEN` scenario.

### 4.1 Capability: Connect & Inspect Server

#### Requirement: The system SHALL verify connectivity and report server metadata
The `connect` subcommand SHALL establish a session and print application metadata, the namespace
table, and all available endpoints with their security policies. **[Confirmed]** (`src/cli/connect.rs`, test `connect_prints_server_metadata`).

##### Scenario: Connect to a reachable server
- **WHEN** `opcua-scan connect opc.tcp://host:4840` is run against a reachable server
- **THEN** stdout includes server application name/URI, the namespace list, and an `Endpoints`
  section listing each endpoint's security policy/mode
- **AND** the process exits `0`.

##### Scenario: Reject a non-OPC-UA endpoint
- **WHEN** the endpoint does not start with `opc.tcp://` (e.g. `http://not-opcua`)
- **THEN** the process fails with exit code `2` (invalid usage/arguments). **[Confirmed]** (test `invalid_endpoint_exits_with_usage_code`).

### 4.2 Capability: Browse / Discover Nodes

#### Requirement: The system SHALL traverse the address space concurrently and safely
`browse` SHALL traverse hierarchical references using a bounded worker pool, following continuation
points, retrying transient failures with backoff, protecting against cycles, and honoring a depth
limit (`--depth N|unlimited`, default `3`). **[Confirmed]** (`src/scanner/engine.rs`).

##### Scenario: Discover known variables
- **WHEN** `browse <endpoint> --depth unlimited --output json` runs against the test server
- **THEN** the JSON array contains records whose `browse_name` includes `Temperature`, `Speed`, and
  `Motor`. **[Confirmed]** (test `browse_discovers_known_variables`).

##### Scenario: Respect the configured depth limit
- **WHEN** `--depth N` is given
- **THEN** a node at depth `d` SHALL have its children browsed only if `d < N` (root = depth 0);
  `unlimited` (or `-1`) removes the limit. **[Confirmed]** (`Depth::allows_children_at`).

##### Scenario: Cap children per branch
- **WHEN** `--max-children N` is set and a branch exceeds `N` children
- **THEN** the branch is truncated, the truncation is counted in stats, and traversal continues. **[Confirmed]**

#### Requirement: The system SHALL filter nodes during traversal
Filtering SHALL combine path patterns (glob/regex include/exclude) with attribute predicates (node
class, datatype, namespace, readable/writable/historizing, subscribable). Exclude patterns SHALL
prune branches (stop descent); include patterns SHALL gate emission; attribute predicates SHALL be
the final emission gate. **[Confirmed]** (`src/filters/*`, `src/scanner/engine.rs`).

##### Scenario: Type filter returns only the requested classes
- **WHEN** `browse ... --type Variable --output json`
- **THEN** every emitted record has `node_class == "Variable"`. **[Confirmed]** (test `type_filter_returns_only_variables`).

##### Scenario: Subscribable filter
- **WHEN** `browse ... --subscribable --output json`
- **THEN** every record has `subscribable == true`, where *subscribable* is the heuristic
  "node is a `Variable` AND its `AccessLevel` has the CurrentRead bit set." **[Confirmed]** (`is_subscribable`, test `subscribable_filter_finds_variables`).

##### Scenario: Glob path semantics
- **WHEN** an include/exclude glob is provided
- **THEN** `/` is the path separator, `*` matches within a segment, `**` matches across segments,
  and browse paths use **namespace URIs**, not session-local numeric indexes. **[Confirmed]** (README + `src/filters/pattern.rs`).

#### Requirement: The system SHALL optionally preview values and validate subscriptions
- `--preview-values` SHALL read the current value of matched variables. **[Confirmed]**
- `--validate-subscription` SHALL attempt to create a temporary monitored item per subscribable
  node and record the result in `validation_status`, demoting `subscribable=false` on failure. **[Confirmed]** (`src/subscription/mod.rs::validate_records`).

#### Requirement: The system SHALL support watch mode
`--watch <DURATION>` SHALL repeat the scan periodically until interrupted. **[Confirmed]**

### 4.3 Capability: Search

#### Requirement: The system SHALL fuzzy-search the address space
`search <endpoint> <QUERY>` SHALL scan and fuzzy-match each node's name/path/description against the
query, returning the top `--limit` results (default `50`). **[Confirmed]** (`src/cli/search.rs`).

##### Scenario: Find by partial name
- **WHEN** `search <endpoint> Temp --depth unlimited --output json`
- **THEN** a record with `browse_name == "Temperature"` appears in the results. **[Confirmed]** (test `search_finds_temperature`).

### 4.4 Capability: Sample (live)

#### Requirement: The system SHALL subscribe and print live updates for a duration
`sample` SHALL force `subscribable=true`, discover matching variables, subscribe to them, and print
`path = value` lines for the configured `--duration` (default `10s`). **[Confirmed]** (`src/cli/sample.rs`, `run_sample`).

### 4.5 Capability: Acquire (config-driven data acquisition)

#### Requirement: The system SHALL acquire data per a TOML config
`acquire --config <FILE>` (or `--config-dir <DIR>`) SHALL connect once, resolve a node selection,
partition nodes into poll vs subscribe by **effective mode**, and run scan/poll/subscribe/history
tasks concurrently, emitting samples to a `Sink`. **[Confirmed]** (`src/acquire/mod.rs`, `src/cli/acquire.rs`).

##### Scenario: Per-node mode routing
- **WHEN** a node has an explicit `mode` (`subscribe`/`poll`)
- **THEN** that mode wins; otherwise `auto` subscribes iff the node is subscribable and the server
  supports monitoring it, else polls; `--poll-only` / `[acquire].poll_only` forces poll for all. **[Confirmed]** (`effective_subscribe`, unit tests in `acquire/mod.rs`).

##### Scenario: Default mode inference
- **WHEN** `[acquire].default_mode` is omitted
- **THEN** it is inferred from tasks present: only a poll task → poll all; only a subscribe task →
  subscribe all; otherwise `auto`. **[Confirmed]** (`effective_default_mode`).

##### Scenario: thin-edge output from polling
- **WHEN** an acquire config uses `[output].format = "tedge"` with a `poll` task
- **THEN** each reading is emitted as a thin-edge.io measurement object `{ "time": ..., "<key>": <value>, "meta": {...} }`. **[Confirmed]** (test `acquire_poll_emits_tedge_measurements`, `src/acquire/sink.rs`).

#### Requirement: The system SHALL cache reconstructed browse paths back into the config
On first run, missing `path` values for explicit `[[selection.node]]` entries SHALL be reconstructed
and written back to the config file (preserving formatting via `toml_edit`). **[Confirmed]** (`persist_node_paths`, `update_node_paths_in_toml`).

### 4.6 Capability: Interactive TUI

#### Requirement: The system SHALL provide a lazy interactive explorer
`browse --tui` SHALL fetch children only when a node is expanded, let the user mark nodes/subtrees
for acquisition (`auto`/`subscribe`/`poll`), preview values, validate subscriptions, export loaded
nodes, and generate an `acquire` config — always restoring the terminal on exit (including panic). **[Confirmed]** (`src/tui/mod.rs`, README key table).

### 4.7 Capability: Output & Export

#### Requirement: The system SHALL provide human and machine output channels
- Human: `--format tree|table|json` (default `tree`). **[Confirmed]**
- Machine: `--output json|ndjson|csv` to stdout or `--file`. CSV SHALL use a fixed column set. **[Confirmed]** (`src/export/*`).
- All logging SHALL go to stderr; stdout SHALL carry only the requested output. **[Confirmed]** (README, `src/logging.rs`).

### 4.8 Capability: Connection Security & Auth

#### Requirement: The system SHALL support OPC UA security and authentication
The system SHALL support security policies `None`, `Basic256Sha256`, `Aes128Sha256RsaOaep`,
`Aes256Sha256RsaPss`; modes `None`, `Sign`, `SignAndEncrypt`; X.509 (`--cert`/`--key`),
user/password (`--user`/`--password`, with `env:VAR` or secure prompt), and anonymous. Server certs
SHALL be auto-trusted only with `--trust-server`. Secrets SHALL never be logged. **[Confirmed]** (`src/opcua/client.rs`).

### 4.9 Capability: Profiles, Cache, Logging, Signals

#### Requirement: The system SHALL merge profile defaults under CLI flags
`--profile NAME|PATH` SHALL load a YAML profile and fill only **unset** connection/traversal/filter
values; explicit CLI flags SHALL always win. **[Confirmed]** (`src/config/mod.rs`).

#### Requirement: The system SHALL provide stable exit codes
`0` success, `1` general failure, `2` invalid usage/args, `3` connection failure, `4` cancelled
(Ctrl+C). **[Confirmed]** (`src/error.rs::exit_code`, `main.rs`).

##### Scenario: Graceful cancellation
- **WHEN** the user presses Ctrl+C once
- **THEN** the run cancels gracefully and exits `4`; a second Ctrl+C exits immediately (`130`). **[Confirmed]** (`src/signal.rs`).

---

## 5. Data Model / Domain Entities

The canonical record types. Full field lists are in the component docs.

| Entity | Where | Purpose |
| --- | --- | --- |
| `NodeRecord` | `src/export/record.rs` | A discovered node for display/export (browse_path, names, node_id, namespace_uri, node_class, subscribable, + optional datatype/value_rank/access_level/historizing/validation_status/current_value/engineering_units/description). **[Confirmed]** |
| `NodeAttributes` | `src/opcua/attributes.rs` | Batch-read attribute bundle (node_class, names, description, data_type, value_rank, access levels, historizing). **[Confirmed]** |
| `NamespaceTable` | `src/opcua/namespaces.rs` | Bi-directional namespace index ↔ URI map. **[Confirmed]** |
| `Sample` | `src/acquire/sink.rs` | An emitted reading (name, node_id, value, time, status, shared `meta`). **[Confirmed]** |
| `SampleMeta` | `src/acquire/sink.rs` | Descriptive metadata (data_type, units, browse/display name, path, namespace, eu_range, source). **[Confirmed]** |
| `ValueReading` / `ValueSnapshot` | `src/opcua/value.rs` | Read value with/without timestamps + status. **[Confirmed]** |
| `AcquireConfig` (+ sub-tables) | `src/acquire/config.rs` | The acquire TOML schema (connection, acquire, selection, output, tasks). **[Confirmed]** |
| `Profile` | `src/config/mod.rs` | YAML saved defaults for a server. **[Confirmed]** |
| `ScanStats` / `StatsSnapshot` | `src/scanner/stats.rs` | Traversal counters. **[Confirmed]** |

See [components/export.md](components/export.md) for the exact `NodeRecord` JSON contract.

---

## 6. API Contracts / Interfaces

This project has **no network API of its own**; its public interface is the CLI plus the file
formats it reads/writes.

- **CLI surface:** see [components/cli.md](components/cli.md) for every subcommand, flag, default,
  and value enum. **[Confirmed]**
- **Acquire config (TOML):** see [components/acquire.md](components/acquire.md). **[Confirmed]**
- **Profile (YAML):** see [components/config-profiles.md](components/config-profiles.md). **[Confirmed]**
- **Export records (JSON/NDJSON/CSV):** see [components/export.md](components/export.md). **[Confirmed]**
- **Outbound integration:** OPC UA over `opc.tcp://` via `async-opcua`; optional downstream
  thin-edge.io via stdout piped to `tedge mqtt pub`. **[Likely]**

---

## 7. Dependencies & External Services

See [architecture.md §Dependencies](architecture.md#dependencies) for the full list. Highlights **[Confirmed]** from `Cargo.toml`:

- **OPC UA:** `async-opcua 0.18` (client; server only in dev for tests).
- **Async:** `tokio` (full), `tokio-util`, `futures`.
- **CLI/serde:** `clap 4.5` (derive/env/wrap_help), `serde`, `serde_json`, `serde_yaml`, `toml`,
  `toml_edit`, `csv`.
- **Filtering:** `globset`, `regex`, `fuzzy-matcher`.
- **Observability/UX:** `tracing`, `tracing-subscriber`, `indicatif`.
- **Errors/util:** `thiserror`, `anyhow`, `humantime`, `dirs`, `rpassword`.
- **TUI:** `ratatui 0.28`, `crossterm 0.28` (used only when `--tui`).

**External services:** an OPC UA server (required at runtime). No databases, queues, or background
job systems. **[Confirmed]**

---

## 8. Environment / Configuration Requirements

- **Toolchain:** stable Rust ≥ 1.88 (edition 2024). **[Confirmed]**
- **Env vars:**
  - `OPCUA_PW` / arbitrary `env:VAR` referenced by `--password env:VAR` or config `password`. **[Confirmed]**
  - `OPCUA_TEST_ENDPOINT` — point integration tests at an external server. **[Confirmed]**
  - `RUST_LOG` — honored by `tracing-subscriber` env-filter (in addition to `-v`/`-q`). **[Likely]**
- **PKI directory:** `--pki-dir` holds `own/`, `private/`, `trusted/`, `rejected/`; a sample keypair
  is auto-created. The repo ships a `pki/` tree. **[Confirmed]** (workspace + `create_sample_keypair(true)`).
- **Profiles:** resolved from `<config-dir>/opcua-scan/profiles/<name>.{yaml,yml}` or
  `~/.opcua-scan/profiles/<name>.{yaml,yml}`. **[Confirmed]**
- **Cache:** `--cache DIR` stores `<sanitized-endpoint>.namespaces.json`. **[Confirmed]**

---

## 9. Deployment / Runtime Assumptions

- **Distribution:** a single statically-strippable binary (`profile.release` uses thin LTO,
  `codegen-units = 1`, `strip = true`). Built via `cargo build --release` → `target/release/opcua-scan`. **[Confirmed]**
- **Execution model:** short-lived CLI invocations, except `acquire` (and `sample`/`--watch`) which
  run continuously until a duration elapses or Ctrl+C. **[Confirmed]**
- **No container/CI manifests found** in the repo (no Dockerfile, no `.github/workflows`). Packaging
  and CI are **[Assumption / needs validation]** — see [assumptions.md](assumptions.md).
- **thin-edge.io integration:** `acquire` output is designed to be piped into the thin-edge.io
  ecosystem (the repo lives under a `thin-edge.io` project tree and emits `tedge` measurement
  objects). **[Likely]**

---

## 10. Testing Strategy

- **Integration tests** (`tests/cli.rs` + `tests/common/mod.rs`): spin up an **in-process OPC UA
  server** exposing `Objects/Factory/Line1/Motor/{Temperature(Double,25.0), Speed(Float,1500.0)}`,
  then invoke the compiled binary and assert on stdout/exit codes. Covers connect, browse, type
  filter, subscribable filter, search, CSV export, invalid-endpoint exit code, and acquire→tedge. **[Confirmed]**
- **External-server mode:** set `OPCUA_TEST_ENDPOINT` to run the same tests against a real server. **[Confirmed]**
- **Unit tests:** present in `acquire/mod.rs` (mode routing/partitioning) and likely across filter/
  config modules. **[Confirmed for acquire; Likely elsewhere]**
- **Dev workflow:** `cargo fmt`, `cargo clippy --all-targets`, `cargo test`. **[Confirmed]** (README).

Recommended additions are listed in [assumptions.md](assumptions.md) (e.g. property-based filter
tests, security-mode coverage, history task tests).

---

## 11. Known Gaps / Assumptions / Inferred Behavior

Consolidated in [assumptions.md](assumptions.md). Top items:
- No CI/Docker/packaging configuration found in-repo. **[Assumption]**
- MQTT publishing is out of scope; `[output].topic` is only a prefix hint for downstream tooling. **[Likely]**
- The `subscribable` heuristic is intentionally minimal (readable Variable); some servers may report
  readable variables that still reject monitoring — hence `--validate-subscription`. **[Confirmed]**
- Namespace **indexes** are session-local; the tool standardizes on namespace **URIs** in paths and
  remaps explicit node ids by URI. **[Confirmed]**

---

## 12. Repository Map (top-level)

| Path | Purpose | Evidence |
| --- | --- | --- |
| `Cargo.toml` | Crate manifest, deps, release profile, binary def. | [Confirmed] |
| `README.md` | User-facing docs (commands, flags, examples). | [Confirmed] |
| `src/main.rs` | Entry point: parse CLI, init logging, apply profile, dispatch, map exit codes. | [Confirmed] |
| `src/cli/` | clap definitions + per-command `run()` (`connect`, `browse`, `search`, `sample`, `acquire`). | [Confirmed] |
| `src/opcua/` | async-opcua wrapper: client/session, browse, attributes, value, namespaces, history. | [Confirmed] |
| `src/scanner/` | Concurrent traversal engine + stats. | [Confirmed] |
| `src/filters/` | Path patterns + attribute predicates → `FilterSet`. | [Confirmed] |
| `src/export/` | `NodeRecord` + tree/table/json/ndjson/csv writers. | [Confirmed] |
| `src/acquire/` | Config schema, selection, sinks, orchestration, config generation. | [Confirmed] |
| `src/subscription/` | Monitored items: validate, sample, live subscription→sink. | [Confirmed] |
| `src/tui/` | Interactive ratatui explorer + acquire-config generation. | [Confirmed] |
| `src/config/` | `Profile` (YAML) load + merge. | [Confirmed] |
| `src/cache/` | Best-effort namespace-table cache. | [Confirmed] |
| `src/error.rs` | `ScanError` + `exit_code()`. | [Confirmed] |
| `src/logging.rs`, `src/signal.rs` | tracing init; Ctrl+C handler. | [Confirmed] |
| `tests/`, `tests/common/` | Integration tests + in-process server fixture. | [Confirmed] |
| `examples/acquire-demo.toml`, `test.toml` | Example/working acquire configs. | [Confirmed] |
| `pki/`, `tmp/`, `opcua-scan-export.json` | Sample PKI tree, scratch, sample export output. | [Confirmed] |

Per-component detail: [components/](components/).
