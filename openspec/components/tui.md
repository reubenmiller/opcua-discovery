# Component: Interactive TUI (`src/tui/`)

> Opt-in `browse --tui` explorer: lazy browsing, marking, export, and acquire-config generation.
> Evidence: **[Confirmed]** (structure summarized; file is large).

## Entry
- **`TuiOptions`** `{ root: NodeId, page_size, export_file: Option<PathBuf>, strategy, workers,
  max_children, retry, preview_values, connection: GenConnection, acquire_file: PathBuf,
  selected_ids: HashSet<String>, selected_paths: HashSet<String>, source_config: Option<PathBuf> }`.
- `TuiOptions::run(session, namespaces, filters, options) -> Result<()>` — launches the loop; the
  terminal is always restored on exit, **including panic**.

## App state (`App`)
- `session, namespaces, filters` — live connection + metadata.
- `arena: Vec<Node>` — lazily-built tree; `visible: Vec<usize>` — flattened expanded chain.
- `selected: usize`, `list_state` — current selection.
- `filter_query: String`, `filter_mode: bool` — live path search (`/`).
- `marked: Vec<usize>` — logical marks (a mark = node + whole subtree).
- `mode: NodeMode` — acquisition mode applied to new marks (Auto/Subscribe/Poll).
- `vars_only: bool` — hide non-Variable containers.
- `job, job_cancel, pending_job` — background work (browse/export/collect).
- `Node { loaded, loading, marked, ... }` — lazy-load flags.

## Key bindings
| Key | Action |
| --- | --- |
| `↑/↓`, `k/j` | Move selection |
| `→/Enter/l` | Expand (lazy browse children on demand) |
| `←/h` | Collapse / parent |
| `space` | Mark node/subtree (auto mode) |
| `S` / `P` | Mark to subscribe / poll |
| `V` | Toggle vars-only |
| `v` | Preview selected variable's value |
| `s` | Validate subscription for selected variable |
| `e` | Export loaded/marked nodes (uses `--file` or `opcua-scan-export.json`) |
| `a` | Generate/update `acquire` config from marked variables |
| `t` | Write/merge the tedge-dot point library (`--library`) from marked variables |
| `x` | Cancel in-progress job |
| `?` | Help overlay (all keys + mark symbols; `↑`/`↓` scroll, `?`/`Esc`/`q` close) |
| `/` | Incremental path filter |
| `g` / `G` | Jump top / bottom |
| `q` / `Esc` / `Ctrl+C` | Quit |

## Key legend and help
`KEY_HELP` is the single table of browsing keys: the legend row (above the status line) and the
`?` overlay are both rendered from it. `fit_legend` keeps entries by priority for the terminal width
and always appends `? help`; filter input, the acquire prompt and the overlay have their own short
entry lists, and a running job adds `x cancel`. The overlay is two columns when the terminal is at
least 96 wide, scrolls when taller than the screen, and captures keys via `help_open`.

## Main loop
Render (tree + detail pane + progress/footer) → select over terminal input, background job messages
(`BrowseDone`/`ExportDone`/`CollectDone`), and a periodic tick (spinner) → handle input → process
job results (attach children, refresh visible, reposition selection).

## Lazy browsing
On expand, if a node isn't `loaded`, spawn a non-blocking `browse_into_children` job
(`opcua::browse::browse_children`); attach results on completion. Marks are logical/instant; export
and acquire traverse into unmarked nodes only as needed.

## Export / Acquire jobs
- **Export:** recursively traverse each marked root, build `NodeRecord`s for Variables, write
  json/ndjson/csv.
- **Acquire:** collect readable Variables under marks, then either `update_selection_in_toml`
  (editing an existing `source_config`) or `generate_toml` (new config); if editing, prompt for
  `<poll|subscribe> <interval>`. See [acquire.md](acquire.md) `generate.rs`.
- **Library (`t`):** same background collection (`collect_job` yields Variables and Methods as
  `(NodeRecord, mode)`; `CollectFor` says what it is for), then a discovery job
  (`tedge_dot::schema::discover`, `JobMsg::LibraryReady`), then `tedge_dot::library::write`. The
  main loop picks up a job started from a job message. The removal set is the
  loaded Variables that are neither marked nor under a mark, computed when `t` is pressed.

## Integrations
Uses `opcua::browse` (lazy children), `opcua::value` (preview), `subscription` (validate),
`export` (`NodeRecord`), and `acquire::generate` (config emit).

## Extension points
- New key action → extend the input handler + (optionally) a new background job message.
- New marking semantics → extend `Node.marked` handling and the collect/export traversal.
