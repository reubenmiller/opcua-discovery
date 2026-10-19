# Proposal

## Why

Two defects showed up when acquiring the demo pump from its tedge-dot config:

- `opcua-scan acquire --duration 30s` always exits with code 4, the code for an interrupted run
  (Ctrl+C), because the duration timer sets the same cancel flag as the signal handler. Scripts
  cannot tell a completed timed run from an aborted one.
- Samples from servers that send neither a source nor a server timestamp (python-asyncua, the
  demo pump) have `"time": null`, which makes NDJSON/CSV output and later analysis useless.

## What Changes

- A run that ends because its `--duration` elapsed exits with code 0; Ctrl+C keeps exit code 4.
- Every sample carries a time: the source timestamp, else the server timestamp, else the time the
  client received the value.

## Capabilities

### New Capabilities
- `acquire-run`: how an acquire run ends (duration vs interrupt, exit codes) and the timestamp each
  sample carries.

### Modified Capabilities
<!-- none -->

## Impact

- Code: `src/acquire/mod.rs` (run-local stop flag), `src/opcua/value.rs` (time fallback).
- Tests: unit test for the fallback; e2e tests require exit code 0 for timed runs.
- Users: scripts checking `$?` after `--duration` now see 0.
