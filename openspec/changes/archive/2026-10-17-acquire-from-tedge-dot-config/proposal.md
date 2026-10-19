# Proposal

## Why

A device's point list now lives in tedge-dot's format (inline `[[device.point]]` and `points_from`
libraries, written by `browse --output tedge-dot` and the TUI's `t`). `opcua-scan acquire` still
needs its own config with the same nodes repeated in `[[selection.node]]` and its own connection.
That is two lists to keep in sync. A Copilot PR (#1 on the fork, closed) tried to fix this by
giving the acquire config a tedge-dot-like syntax, but the result only looked compatible: tedge-dot
rejects it (`acquire`/`output`/`task` tables, a point key `path`, ids with `-`), and it duplicated
`--tedge-dot-config`. Its namespace-safety rule was sound and is kept here.

## What Changes

- `opcua-scan acquire --tedge-dot-config <FILE> [--device <NAME>]` acquires the device's points
  directly from the real tedge-dot connector config: connection as for every other command,
  points from `points_from` libraries (located with tedge-dot's search rules) and inline points
  (overriding by `id`), each node id translated through its namespace table
  (index → URI → server index; a URI missing on the server fails the run, never a numeric
  fallback).
- `subscribe = false` points are polled, the others follow `auto`. Points acquire cannot read as
  tedge-dot does (methods, write-only, disabled, `field`/`index`) are skipped and reported.
- Samples are keyed by the point `id`, as tedge-dot publishes them.
- `--config <acquire.toml>` may be added for acquire-only settings: `[acquire]`, `[output]` and
  `[[task]]` (history, scan, intervals, CSV/JSON files). `[connection]` or `[selection]` in it is an
  error. Without it, a poll task uses the device's (or connector's) `poll_interval` (default 2s)
  and a subscribe task its `sampling_interval`.
- The tedge-dot config is only read, never written (no browse-path caching into it).

## Capabilities

### New Capabilities
- `tedge-dot-acquire`: running `acquire` from a tedge-dot connector config: point resolution,
  namespace translation, skip rules, output keys, the optional extras file and the defaults.

### Modified Capabilities
<!-- none: tedge-dot-connection is reused unchanged -->

## Impact

- Code: new `src/tedge_dot/points.rs` (device + library point resolution); `src/cli/acquire.rs`
  (flags, building the config); `src/acquire/mod.rs` (`run` takes the connection arguments and an
  optional write-back path); `src/acquire/config.rs` (extras file, `NodeSelector` key);
  `src/acquire/sink.rs` + `selection.rs` (output key override).
- Tests: unit tests for resolution/translation/skips; e2e acquire runs against the test server
  with a tedge-dot config, a library and an extras file.
- Docs: README, `openspec/components/acquire.md`.
- Users: additive; existing acquire configs are unchanged.
