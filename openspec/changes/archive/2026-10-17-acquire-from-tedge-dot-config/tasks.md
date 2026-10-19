# Tasks

## 1. Point resolution

- [x] 1.1 `src/tedge_dot/points.rs`: device selection (shared with connection.rs), library search path and `locate`, merge by id, namespace table per point, skip rules, intervals
- [x] 1.2 Unit tests: libraries + override, search path order, path references, tables, skips, defaults

## 2. Acquire

- [x] 2.1 `NodeSelector.key` → `SampleMeta.key` → `output_key`
- [x] 2.2 `acquire::run` takes `ConnectionArgs` and an optional write-back path
- [x] 2.3 `AcquireArgs`: `--tedge-dot-config`, `--device`; extras file with table check; default tasks

## 3. Tests and docs

- [x] 3.1 E2E: acquire from a tedge-dot config with a library (renumbered table), extras with a history/poll task, refusal of `[selection]`, config unchanged
- [x] 3.2 README, `openspec/components/acquire.md`; `just ci`; archive
