# Assumptions, Gaps & Items to Validate

> Explicit uncertainty register for the reverse-engineered OpenSpec. Each item is labeled
> **[Confirmed]**, **[Likely]**, or **[Assumption]**. Validate **[Assumption]** items before relying
> on them.

## A. Build, CI & packaging
1. **CI added** (`.github/workflows/ci.yml` + `nightly.yml`). PR CI runs fmt, clippy
   (`-D warnings`), the hermetic test matrix (ubuntu/macos stable + ubuntu MSRV 1.88) and
   coverage; the nightly workflow runs the Docker-based interoperability suite. **[Resolved]**
2. **Docker for interop only**: `tests/interop/docker-compose.yml` brings up reference
   servers (Microsoft OPC-PLC, node-opcua; opt-in open62541/Milo/refserver). The product
   itself still ships as a bare binary — no runtime container. **[Resolved/Assumption]**
3. **No release/packaging automation** (e.g. cargo-dist, cross) found. Distribution method is
   unverified beyond `cargo build --release`. **[Assumption]**
4. Release profile (`lto=thin`, `codegen-units=1`, `strip=true`) implies binaries are intended for
   distribution. **[Likely]**

## B. thin-edge.io integration
5. The crate emits `tedge` measurement objects and lives under a `thin-edge.io` project tree, so the
   `acquire` output is **intended to be piped to the thin-edge.io ecosystem** (e.g. `tedge mqtt
   pub`). The crate itself has **no MQTT client** (`[output].topic` is only a prefix hint). **[Likely]**
   → Confirm the exact downstream wiring (systemd unit? a wrapper that pipes stdout to `tedge`?).
6. The `tedge` JSON shape (`{ time, <key>: value, meta }`) is assumed to match thin-edge's measurement
   schema. **[Likely]** → Validate against the target thin-edge version.

## C. OPC UA behavior
7. **Subscribable heuristic** is deliberately minimal: a readable `Variable`. Servers may still reject
   monitoring; use `--validate-subscription` / `auto` fallback to poll. **[Confirmed]**
8. **Namespace indexes are session-local**; the tool standardizes on **URIs** in paths and remaps
   explicit node ids by URI. Legacy `node_ids` (bare strings) are **not** remapped. **[Confirmed]**
9. Retryable status set (`BadTimeout`, `BadTooManyOperations`, `BadServerTooBusy`,
   `BadNoContinuationPoints`, `BadServerNotConnected`) is the engine's definition of "transient".
   Other errors are counted and skipped, not retried. **[Confirmed]**
10. `create_sample_keypair(true)` auto-generates a client keypair in the PKI dir when absent. The
    committed `pki/` tree is a sample; production deployments should supply their own. **[Confirmed/Assumption]**
    → Do **not** rely on committed certs for production.

## D. Configuration semantics to double-check
11. `[acquire].default_mode` inference (poll-only→poll, subscribe-only→subscribe, else auto) is the
    behavior when omitted. **[Confirmed]**
12. `acquire --poll-only` and `[acquire].poll_only` are **monotonic**: once enabled they cannot be
    turned off later in the same run. **[Confirmed]**
13. `--config-dir` loads configs in **sorted** order; behavior on a partially-failing directory run
    (continue vs abort) should be confirmed. **[Likely / needs validation]**
14. Profile resolution order is `<config-dir>/opcua-scan/profiles/` then `~/.opcua-scan/profiles/`
    with `.yaml`/`.yml`. Exact `<config-dir>` is from the `dirs` crate (platform-specific). **[Confirmed]**

## E. Output & formats
15. CSV uses a **fixed** column set for `NodeRecord` export to avoid ragged rows; acquire CSV uses a
    **union-of-keys** header instead. These are two different CSV paths (export vs sink). **[Confirmed]**
16. `MetadataMode::Off` trims ndjson/json records to `time, key, value, nodeId, status`. Confirm this
    matches any strict-schema downstream consumer. **[Confirmed]**

## F. Logging & env
17. `RUST_LOG` is assumed honored via `tracing-subscriber` env-filter alongside `-v`/`-q`. **[Likely]**
    → Confirm precedence between `RUST_LOG` and the `-v` count.
18. OPC UA library logs are suppressed below `-vvv`. **[Confirmed]**

## G. Suggested validations / next steps for contributors
- ~~Add CI (fmt + clippy + test) if not present elsewhere.~~ **[Done — see §A]**
- ~~Add tests for: history task end-to-end, security `Sign`/`SignAndEncrypt` handshakes, profile merge
  precedence, `--config-dir` multi-config behavior, and CSV/ndjson schema stability.~~ Covered by
  `tests/security.rs`, `tests/services.rs` and the new unit suites; full HistoryRead end-to-end is
  exercised via interop (the in-process fixture's node manager does not serve HistoryRead). **[Mostly done]**
- Document the exact thin-edge.io downstream pipeline (the missing link between this binary's stdout
  and the cloud).
- Consider exposing subscription keep-alive/lifetime counts and history pagination size via config.

## H. Things explicitly confirmed (not gaps, listed for confidence)
- Five stable exit codes (`0/1/2/3/4`) and Ctrl+C semantics. **[Confirmed]**
- stdout-is-data / stderr-is-logs separation. **[Confirmed]**
- Bounded-concurrency, continuation-point, cycle-safe traversal. **[Confirmed]**
- In-process test server fixture with a known address space. **[Confirmed]**
- Hermetic test suite (unit + in-process integration) plus an opt-in cross-implementation
  interoperability suite and GitHub Actions CI. See `docs/TESTING.md`. **[Confirmed]**
