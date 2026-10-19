# opcua-scan

A fast, production-grade command-line tool for **scanning and discovering an OPC UA
namespace**. It connects to an OPC UA server, browses the address space efficiently,
and reports nodes with rich metadata, powerful filtering, subscribable detection, and
multiple export formats.

`opcua-scan` is built for real industrial environments where address spaces can contain
**10,000–500,000+ nodes**, so traversal is concurrent, cycle-safe, paginated, and
streamed to keep memory low and avoid overloading the server.

Built in Rust on top of [`async-opcua`](https://github.com/freeopcua/async-opcua).

---

## Features

- **Four commands**: `connect`, `browse` (primary), `search`, `sample`.
- **Concurrent traversal**: BFS or DFS with a bounded worker pool, continuation-point
  pagination, retry with backoff on transient failures, and cycle protection.
- **Depth control**: limit traversal depth or go `unlimited`.
- **Rich filters**: include/exclude by glob or regex on the browse path, node class,
  data type, namespace, access level (readable/writable/historizing), and a
  subscribable heuristic.
- **Subscribable detection** and optional **subscription validation** (actually creates a
  temporary monitored item to confirm a node can be monitored).
- **Value preview** for variables.
- **Output**: human-readable `tree` / `table` / `json`, and machine-readable
  `json` / `ndjson` / `csv` exports to stdout or a file.
- **Interactive TUI** (opt-in) for lazy, on-demand exploration.
- **Watch mode** to re-scan periodically.
- **Profiles** to save per-server defaults, and a best-effort **metadata cache**.
- **Secure by default**: the server certificate is only auto-trusted with
  `--trust-server`; secrets are never logged.

---

## Installation

```bash
# From the repository root
cargo build --release
# Binary at target/release/opcua-scan
```

Requires a recent stable Rust toolchain (edition 2024, rustc ≥ 1.88).

---

## Quick start

```bash
# Verify connectivity and print server metadata + endpoints
opcua-scan connect opc.tcp://localhost:4840

# Browse the first few levels and print a tree
opcua-scan browse opc.tcp://localhost:4840 --depth 3

# Find all subscribable variables and export to CSV
opcua-scan browse opc.tcp://localhost:4840 \
  --depth unlimited --subscribable \
  --output csv --file variables.csv
```

---

## Commands

### `connect`

Verify connectivity and print server application metadata, the namespace table, and all
available endpoints with their security policies.

```bash
opcua-scan connect opc.tcp://10.0.0.10:4840
```

### `browse` (primary)

Traverse the address space and report matching nodes.

```bash
# Tree view, depth 4
opcua-scan browse opc.tcp://localhost:4840 --depth 4

# Only Variables under a Factory path, as a table, with current values
opcua-scan browse opc.tcp://localhost:4840 \
  --depth unlimited \
  --include "Objects/Factory/**" \
  --type Variable \
  --preview-values \
  --format table

# Exclude a noisy subtree, keep only Double variables, validate subscriptions
opcua-scan browse opc.tcp://localhost:4840 \
  --depth unlimited \
  --exclude "**/Diagnostics/**" \
  --type Variable --datatype Double \
  --subscribable --validate-subscription \
  --output ndjson --file nodes.ndjson

# Depth-first traversal with more workers and a per-branch child cap
opcua-scan browse opc.tcp://localhost:4840 \
  --strategy dfs --workers 16 --max-children 5000 --depth unlimited

# Re-scan every 30 seconds until Ctrl+C
opcua-scan browse opc.tcp://localhost:4840 --depth 3 --watch 30s
```

### `search`

Fuzzy-search the address space for nodes whose name, path, or description matches a query.

```bash
opcua-scan search opc.tcp://localhost:4840 "temp" --depth unlimited
opcua-scan search opc.tcp://localhost:4840 "motor speed" --limit 20 --output json
```

### `sample`

Subscribe to matching variables and print live value updates for a duration.

```bash
# Sample every subscribable variable under a path for 15 seconds
opcua-scan sample opc.tcp://localhost:4840 \
  --include "Objects/Factory/Line1/**" \
  --depth unlimited --duration 15s
```

---

## Interactive TUI

Add `--tui` to `browse` to launch an interactive explorer that browses **lazily** —
children are fetched from the server only when you expand a node, keeping server load
minimal on huge address spaces.

```bash
opcua-scan browse opc.tcp://localhost:4840 --tui
```

A key legend under the tree always shows the keys that apply right now (browsing, filter input,
the acquire prompt, `x cancel` while a job runs); press `?` for an overlay with every key and the
meaning of the mark symbols (`↑`/`↓` scroll it on small terminals, `?`/`Esc`/`q` close it).

Keys:

| Key | Action |
| --- | --- |
| `↑` / `↓` or `k` / `j` | Move selection |
| `→` / `Enter` / `l` | Expand (browse children on demand) |
| `←` / `h` | Collapse / go to parent |
| `space` | Mark node/subtree (auto mode) for acquisition |
| `S` | Mark node/subtree to **subscribe** |
| `P` | Mark node/subtree to **poll** |
| `v` | Preview the selected variable's value |
| `s` | Validate subscription for the selected variable |
| `e` | Export the loaded nodes (uses `--file`, or `opcua-scan-export.json`) |
| `a` | Write an `acquire` config from the marked nodes |
| `t` | Write the marked nodes as a tedge-dot point library (`--library`) |
| `/` | Incremental path filter |
| `?` | Show / hide all keys and mark symbols |
| `g` / `G` | Jump to top / bottom |
| `q` / `Esc` | Quit |

Marked nodes are written into the generated `acquire` config with their chosen
mode. Nodes marked with `space` (auto) follow the config's default mode —
subscribe when the server supports monitoring the node, otherwise poll.

The terminal is always restored on exit, even on panic.

---

## tedge-dot point libraries

opcua-scan can produce a [tedge-dot](https://github.com/thin-edge/tedge-dot) OPC UA **point
library** (a `[library]` table plus `[[point]]` entries) from the nodes it discovers, and connect
with the settings of a device in a tedge-dot connector config instead of its own flags.

```bash
# Connect exactly like the gateway's connector does (endpoint, security, user/password file,
# the gateway's application certificate). CLI flags still win.
opcua-scan connect --tedge-dot-config /etc/tedge/plugins/ot/opcua.toml --device plc-7

# Write every variable and method below Line1 as a library (merges into acme-line.toml)
opcua-scan browse --tedge-dot-config opcua.toml --device plc-7 --depth unlimited \
    --include "Factory/Line1/**" --type Variable,Method --output tedge-dot --file acme-line.toml

# Pick nodes interactively: mark with space, press t to write/merge point-library.toml
opcua-scan browse --tedge-dot-config opcua.toml --device plc-7 --tui --library acme-line.toml
```

- **Points:** `id` (unique snake_case from the browse path), `name`, `description`, `datatype`
  (Double → `float64`, DateTime/LocalizedText/Guid/... → `string`, ByteString → `bytes`, ...),
  `access` (`read_write`/`write` from AccessLevel ∧ UserAccessLevel), `subscribe = false` for
  readable nodes that are not subscribable, and `address = { node_id = ... }`.
- **Custom datatypes** are read from the server (`DataTypeDefinition`, `HasSubtype`):
  - a **structure** variable becomes one read-only point per mappable leaf field,
    `address = { node_id = ..., field = "Motor.Current" }`, nested structures included;
  - an **enumeration** becomes an `int32` point with `map.cases` from its values to its names
    (`as = "string"` for codes the definition does not list);
  - a **derived type** (e.g. a `Percent` subtype of Double) maps like its base type.
- **Methods** become `access = "call_method"` points with
  `address = { object_id, method_id, inputs = [...] }`, the inputs read from `InputArguments`.
  Include them with `--type Variable,Method` (or no `--type`).
- **Skipped:** arrays, array fields, abstract/unsupported types, inaccessible variables,
  non-executable methods and methods with structure or array arguments; a new library lists
  them in its header comment. An empty result is an error and writes nothing.
- **Namespaces:** `[library] namespaces` records the server's namespace table, so tedge-dot
  translates the ids if a server numbers its namespaces differently.
- **Merging:** an existing library is edited in place: comments and hand-edited points are kept,
  new nodes are appended (in the library's namespace numbering). In the TUI, the library's points
  are pre-marked and a point is removed when its node is shown in the tree and unmarked.
- `[library] type` comes from `--library-type`, else the `--tedge-dot-config` device's `type`.
- From the connector config only the connection is used, with tedge-dot's rules:
  `allow_deprecated_security` unlocks `Basic128Rsa15`/`Basic256`, and `allow_plaintext_password`
  is required for an unprotected password. Server-certificate trust stays in opcua-scan's own
  PKI (`--pki-dir`).

### Acquire a tedge-dot device's points

The same point list drives `opcua-scan acquire`, so there is one list to maintain:

```bash
# Read the device's points (points_from libraries + inline points) for 30 s
opcua-scan acquire --tedge-dot-config /etc/tedge/plugins/ot/opcua.toml --device plc-7 --duration 30s

# Add acquire-only settings: history, other intervals, CSV/NDJSON files
opcua-scan acquire --tedge-dot-config opcua.toml --device plc-7 --config extras.toml
```

- Libraries are found like tedge-dot finds them (`[connector] point_library_path`, else
  `TEDGE_DOT_POINT_LIBRARY_PATH`, else `/etc/tedge/plugins/ot/points.d` and
  `/usr/share/tedge-dot/points.d`, file `<dir>/opcua/<name>.toml`); inline points override
  library points with the same `id`.
- Node ids are translated through their namespace table (library, else device) to the server's
  current index; a namespace the server does not list stops the run.
- `subscribe = false` points are polled, the rest subscribe when they can. Method, write-only,
  disabled and `field`/`index` points are skipped with a warning. Samples are keyed by point `id`.
- `extras.toml` may only contain `[acquire]`, `[output]` and `[[task]]`. Without tasks, a poll task
  uses the device's (else connector's) `poll_interval` (default 2s) and a subscribe task its
  `sampling_interval`. Per-point intervals are not applied. Nothing is written to the tedge-dot
  files.

---

## Acquisition modes: subscribe vs poll

`acquire` configs select **per-node** how each variable is read:

- **subscribe** — the server pushes value changes via an OPC UA monitored item
  (efficient; preferred for most variables).
- **poll** — the client reads the value cyclically at a fixed interval (use for
  servers with limited or unreliable subscription support).
- **auto** (default) — subscribe when the server reports the node as
  monitorable, otherwise fall back to polling.

Set the mode per node, choose a default, or force polling globally:

```toml
[acquire]
default_mode = "auto"   # auto | subscribe | poll  (applied to nodes without a mode)
poll_only    = false    # true disables all subscriptions (same as --poll-only)

[[selection.node]]
id = "ns=3;i=1001"
name = "Temperature"
mode = "subscribe"      # subscribe | poll | auto (omit = auto)

[[selection.node]]
id = "ns=3;i=1002"
name = "LegacyCounter"
mode = "poll"

# Task blocks supply the parameters for each mode; nodes are routed to the
# matching task by their resolved mode (a run can poll and subscribe at once).
[[task]]
mode = "poll"
interval = "1s"

[[task]]
mode = "subscribe"
publishing_interval = "500ms"
```

Force polling for a one-off run regardless of config:

```bash
opcua-scan acquire --config acquire.toml --poll-only
```

When `[acquire].default_mode` is omitted it is inferred from the tasks present
(only a poll task → poll everything; only a subscribe task → subscribe
everything), so existing single-task configs are unchanged.

---

## Filtering reference

| Flag | Meaning |
| --- | --- |
| `--include <GLOB>` | Keep branches whose browse path matches the glob (repeatable). |
| `--include-regex <RE>` | Same, using a regular expression. |
| `--exclude <GLOB>` | Prune branches matching the glob (repeatable). |
| `--exclude-regex <RE>` | Same, using a regular expression. |
| `--type <CLASS,...>` | Node classes: `Object`, `Variable`, `Method`, `ObjectType`, `VariableType`, `ReferenceType`, `DataType`, `View`. |
| `--datatype <TYPE,...>` | Variable data type by builtin name (e.g. `Double`), NodeId, or namespace URI. |
| `--include-ns <URI>` | Restrict to namespace URIs (repeatable). |
| `--ns-uri <URI>` / `--ns-index <N>` | Restrict to a single namespace. |
| `--readable` / `--writable` / `--historizing` | Access-level filters. |
| `--subscribable` | Only variables that look eligible for monitoring. |

Globs use `/` as the path separator and support `*` (within a segment) and `**`
(across segments). Browse paths use **namespace URIs**, not session-local numeric indexes.

---

## Traversal & output reference

| Flag | Default | Meaning |
| --- | --- | --- |
| `--depth <N\|unlimited>` | `3` | Maximum depth (root = 0). |
| `--root <NODEID>` | Objects (`i=85`) | Starting node. |
| `--strategy <bfs\|dfs>` | `bfs` | Traversal order. |
| `--workers <N>` | `8` | Concurrent browse workers. |
| `--page-size <N>` | `200` | References requested per browse call. |
| `--max-children <N>` | — | Cap children per branch; reports truncation. |
| `--format <tree\|table\|json>` | `tree` | Human-readable display. |
| `--output <json\|ndjson\|csv>` | — | Machine-readable export. |
| `--file <FILE>` | stdout | Export destination. |
| `--preview-values` | off | Read current values for matched variables. |
| `--watch <DURATION>` | — | Repeat the scan periodically. |

---

## Connection, security & authentication

| Flag | Meaning |
| --- | --- |
| `--user <USER>` | Username for user/password auth. |
| `--password <PASSWORD\|env:VAR>` | Password; use `env:VAR` to read from an env var, or omit to be prompted securely. Secrets are never logged. |
| `--cert <FILE>` / `--key <FILE>` | Client certificate and private key. |
| `--security-policy <POLICY>` | `None`, `Basic128Rsa15`, `Basic256`, `Basic256Sha256`, `Aes128Sha256RsaOaep`, `Aes256Sha256RsaPss` (any case; tedge-dot spellings such as `Aes128_Sha256_RsaOaep` and the policy URIs work too). |
| `--security-mode <MODE>` | `None`, `Sign`, `SignAndEncrypt`. Default: `SignAndEncrypt` with a security policy, else `None`. |
| `--allow-deprecated-security` | Required for the deprecated SHA-1 policies `Basic128Rsa15` and `Basic256`. |
| `--trust-server` | Auto-trust the server certificate (**insecure**; lab use only). |
| `--pki-dir <DIR>` | Client PKI directory (own cert, key, trusted certs). |
| `--timeout <DURATION>` | Per-request timeout (default `10s`). |
| `--retry <N>` | Retries for transient failures (default `3`). |
| `--cache <DIR>` | Best-effort metadata cache directory. |

```bash
# User/password from an environment variable, signed & encrypted channel
export OPCUA_PW='s3cret'
opcua-scan browse opc.tcp://10.0.0.10:4840 \
  --user operator --password env:OPCUA_PW \
  --security-policy Basic256Sha256 --security-mode SignAndEncrypt \
  --depth 3
```

Every policy async-opcua implements is supported, with anonymous, username/password and X.509
user identities. ECC policies are not available (neither async-opcua nor tedge-dot implements
them). A password that would travel unprotected (a `None` channel, or a signed-only channel
whose username token policy names `None`) is sent with a warning; with `--tedge-dot-config` it is
refused unless the config sets `allow_plaintext_password = true`, as the connector does.

---

## Profiles

Save common settings for a server in a YAML profile, then select it with `--profile NAME`
(a name is resolved under `<config-dir>/opcua-scan/profiles/<name>.yaml` or
`~/.opcua-scan/profiles/<name>.yaml`) or pass a file path directly. **CLI flags always
override profile values.**

```yaml
# ~/.opcua-scan/profiles/plant.yaml
endpoint: opc.tcp://10.0.0.10:4840
user: operator
security-policy: Basic256Sha256
security-mode: SignAndEncrypt
timeout: 15s
retry: 5

depth: unlimited
strategy: bfs
workers: 12

include:
  - "Objects/Factory/**"
exclude:
  - "**/Diagnostics/**"
node-class:
  - Variable
datatype:
  - Double
```

```bash
opcua-scan browse --profile plant --subscribable --output csv --file plant.csv
```

---

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success (also an `acquire` run that ends because its `--duration` elapsed) |
| `1` | General failure |
| `2` | Invalid usage / arguments |
| `3` | Connection failure |
| `4` | Cancelled (Ctrl+C), also during an `acquire --duration` run |

Acquired samples always carry a `time`: the source timestamp, else the server timestamp, else
the time opcua-scan received the value (some servers send neither).

---

## Logging

Logging goes to **stderr** so it never pollutes machine-readable output on stdout.

- `-v` / `-vv` / `-vvv` increase verbosity (info / debug / trace).
- `-q` suppresses everything except errors.

The underlying OPC UA libraries are kept at `warn` unless you pass `-vvv`.

---

## Development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test            # 98 hermetic tests, no network required
```

If you have [`just`](https://github.com/casey/just) installed, the common
workflows are wrapped as recipes (`just test`, `just lint`, `just fmt-check`,
`just servers-up`, `just test-interop`, …). Run `just` to list them.

Integration tests spin up an **in-process OPC UA server** with a known address
space (`Factory/Line1/Motor/...`, including typed, historizing and secured
nodes) and drive the compiled binary against it. To run the tests against an
**external** server instead, set:

```bash
export OPCUA_TEST_ENDPOINT=opc.tcp://localhost:4840
cargo test
```

A cross-implementation **interoperability** suite (`tests/interop.rs`) exercises
the CLI against real third-party servers (open62541, node-opcua, Eclipse Milo,
OPC Foundation reference server, Microsoft OPC-PLC, public demo). It is
`#[ignore]`d by default and runs nightly in CI. See **[docs/TESTING.md](docs/TESTING.md)**
for the full testing guide (test tiers, spinning up reference servers, debugging
failures and adding new interoperability cases).

---

## License

Apache-2.0
