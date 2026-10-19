# Proposal

## Why

The TUI shows its keys only in the initial status line, and the first status message (a browse
result, a mark, an export) replaces them. Users then have to remember or look up the keys, and
several actions (`t` library, `S`/`P`, `V`, `x`) are effectively undiscoverable.

## What Changes

- A permanent **key legend** row above the status line lists the keys that apply right now:
  browsing, typing a filter, the acquire prompt, a running job (`x cancel`). On a narrow terminal
  lower-priority entries are dropped; `? help` always stays.
- **`?` opens a help overlay** with every key binding grouped by purpose and the meaning of the
  mark symbols `[x]`, `[S]`, `[P]`, `[+]`. `?`, `Esc` or `q` closes it; other keys are ignored
  while it is open.
- Status messages no longer carry key lists; they report what happened.

## Capabilities

### New Capabilities
- `tui-key-legend`: the always-visible, context-dependent key legend and the `?` help overlay
  of the interactive explorer.

### Modified Capabilities
<!-- none: the TUI is not yet specified in openspec/specs -->

## Impact

- Code: `src/tui/mod.rs` (layout row, legend and overlay rendering, `?` handling, default status).
- Docs: README TUI section, `openspec/components/tui.md`.
- Users: one more row of the terminal is used by the legend.
