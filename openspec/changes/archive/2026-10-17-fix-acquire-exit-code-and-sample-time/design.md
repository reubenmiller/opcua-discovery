# Design

- `acquire::run` shadows the caller's `cancel` (the process-wide signal flag that `main` turns
  into exit code 4) with a run-local `stop` flag. A small watcher copies the signal flag into
  `stop`; the duration timer sets only `stop`. All tasks observe `stop`. The watcher ends when
  `stop` is set, and `run` sets `stop` before returning so no watcher outlives its run (relevant
  for `--config-dir`, which runs configs one after another).
- `ValueReading::best_time` falls back to `chrono::Utc::now()` in RFC 3339 (the same format the
  server timestamps are rendered in). The fallback is taken when the reading is built, i.e. at
  receive time, not when it is written.
