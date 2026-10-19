//! Interactive terminal explorer (opt-in via `--tui`).
//!
//! Unlike the streaming scan, the TUI browses **lazily**: it only asks the
//! server for a node's children when the user expands that node. This keeps
//! server load minimal while allowing interactive exploration of very large
//! address spaces.
//!
//! Layout: a navigable tree on the left and a detail pane on the right, with a
//! status/help footer. The terminal is always restored on exit or panic via a
//! RAII guard plus a panic hook.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use opcua::client::Session;
use opcua::types::{NodeClass, NodeId, ReferenceDescription};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

use crate::acquire::config::NodeMode;
use crate::acquire::generate::GenMode;
use crate::cli::Encoding;
use crate::error::{Result, ScanError};
use crate::export::{NodeRecord, write_export};
use crate::filters::{FilterSet, is_subscribable};
use crate::opcua::attributes::{NodeAttributes, read_node_attributes};
use crate::opcua::browse::browse_children;
use crate::opcua::namespaces::NamespaceTable;
use crate::opcua::node_class_name;
use crate::opcua::value::read_values;
use crate::subscription;

/// Spinner frames for the background-job progress indicator.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A message sent from a background job back to the UI thread.
enum JobMsg {
    /// Incremental progress: number of nodes processed so far.
    Progress { processed: usize },
    /// A lazy browse (node expansion) finished.
    BrowseDone {
        parent: usize,
        result: std::result::Result<Vec<LoadedChild>, String>,
    },
    /// A background export finished: nodes written (or an error) and the path.
    ExportDone {
        result: std::result::Result<usize, String>,
        path: PathBuf,
    },
    /// A background collection of the Variables below the marked subtrees
    /// (for an acquire config or a point library) finished; each record comes
    /// with the mode of the mark it was found under.
    CollectDone {
        result: std::result::Result<Vec<(NodeRecord, Option<GenMode>)>, String>,
    },
    /// Datatypes and method arguments of the records to write as a point
    /// library were discovered.
    LibraryReady {
        records: Vec<NodeRecord>,
        schema: crate::tedge_dot::schema::Schema,
        remove: std::collections::HashSet<String>,
    },
}

/// A child node produced by a background browse job, ready to insert.
struct LoadedChild {
    node_id: NodeId,
    label: String,
    class: NodeClass,
    path: String,
    expandable: bool,
    attrs: Option<NodeAttributes>,
}

/// UI-side state describing an in-progress background job (for the progress bar).
struct JobState {
    /// Human label for the running job (e.g. "Exporting", "Browsing").
    verb: String,
    /// Nodes processed so far (totals are unknown for streaming work).
    processed: usize,
    /// When false, the job has no measurable unit count (pure spinner).
    counted: bool,
}

/// Options controlling the TUI session.
pub struct TuiOptions {
    /// Root node to anchor the tree at.
    pub root: NodeId,
    /// Browse page size.
    pub page_size: u32,
    /// Default export destination for the `e` action.
    pub export_file: Option<PathBuf>,
    /// Traversal settings reused for the concurrent background export.
    pub strategy: crate::cli::Strategy,
    pub workers: usize,
    pub max_children: Option<usize>,
    pub retry: u32,
    pub preview_values: bool,
    /// Connection summary used to generate an `acquire` config (the `a` action).
    pub connection: crate::acquire::generate::GenConnection,
    /// Destination for a generated `acquire` config (the `a` action).
    pub acquire_file: PathBuf,
    /// Node ids pre-selected from a loaded `acquire` config; these are marked in
    /// the tree as they are encountered while browsing.
    pub selected_ids: std::collections::HashSet<String>,
    /// Browse paths (relative to the root) of the pre-selected nodes, used to
    /// auto-expand the tree to those nodes on startup so they are revealed and
    /// marked without the user having to hunt for them.
    pub selected_paths: std::collections::HashSet<String>,
    /// When set, the `a` action edits this existing config in place (updating
    /// only the selection) instead of generating a fresh file with a prompt.
    pub source_config: Option<PathBuf>,
    /// The tedge-dot point library written by the `t` action (merged into
    /// when it exists; its points are part of `selected_ids`).
    pub library_file: PathBuf,
    /// Settings for a new point library.
    pub library_options: crate::tedge_dot::library::LibraryOptions,
}

/// What a background Variable collection is for.
enum CollectFor {
    /// An acquire config with this default mode and interval.
    Acquire(GenMode, Duration),
    /// The point library; points of these (server) node ids are removed.
    Library {
        remove: std::collections::HashSet<String>,
    },
}

/// Run the interactive explorer until the user quits.
pub async fn run(
    session: Arc<Session>,
    namespaces: Arc<NamespaceTable>,
    filters: Arc<FilterSet>,
    options: TuiOptions,
) -> Result<()> {
    let mut guard = TerminalGuard::new()?;

    let mut app = App::new(session, namespaces, filters, options);
    app.bootstrap().await;
    app.locate_selected().await;

    let mut events = EventStream::new();
    // Receiver for the currently running background job, if any.
    let mut job_rx: Option<mpsc::Receiver<JobMsg>> = None;
    // Animates the progress spinner while a job is running.
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    loop {
        app.ensure_selected_detail().await;
        guard
            .terminal
            .draw(|f| app.render(f))
            .map_err(|e| ScanError::Terminal(e.to_string()))?;

        if app.quit {
            break;
        }

        tokio::select! {
            // Terminal input.
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    app.handle_key(key).await?;
                    // A handler may have launched a background job.
                    if let Some(rx) = app.pending_job.take() {
                        job_rx = Some(rx);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(ScanError::Terminal(e.to_string())),
                None => break,
            },
            // Background job progress / completion.
            msg = recv_job(&mut job_rx) => match msg {
                Some(m) => {
                    if app.handle_job_msg(m) {
                        job_rx = None;
                    }
                    // A finished job may have started the next one.
                    if let Some(rx) = app.pending_job.take() {
                        job_rx = Some(rx);
                    }
                }
                None => {
                    job_rx = None;
                    app.job = None;
                }
            },
            // Periodic redraw to animate the spinner while a job runs.
            _ = tick.tick(), if app.job.is_some() => {
                app.spinner = app.spinner.wrapping_add(1);
            }
        }
    }

    Ok(())
}

/// Await the next job message, or never resolve when there is no active job.
async fn recv_job(rx: &mut Option<mpsc::Receiver<JobMsg>>) -> Option<JobMsg> {
    match rx.as_mut() {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// RAII terminal guard: enters raw mode + alternate screen on construction and
/// always restores the terminal on drop. Also installs a panic hook so a panic
/// inside the UI does not leave the terminal in a broken state.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        // Restore the terminal first if a panic unwinds through the UI.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            default_hook(info);
        }));

        enable_raw_mode().map_err(|e| ScanError::Terminal(e.to_string()))?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).map_err(|e| ScanError::Terminal(e.to_string()))?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).map_err(|e| ScanError::Terminal(e.to_string()))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// A single node in the lazily-built tree.
struct Node {
    node_id: NodeId,
    label: String,
    class: NodeClass,
    path: String,
    depth: usize,
    parent: Option<usize>,
    children: Vec<usize>,
    loaded: bool,
    expanded: bool,
    expandable: bool,
    /// True while a background browse for this node's children is in flight.
    loading: bool,
    /// Whether this node is marked for export (set on the whole subtree).
    marked: bool,
    /// Per-node acquisition mode applied when marked (defaults to auto).
    mode: NodeMode,
    attrs: Option<NodeAttributes>,
    value: Option<String>,
}

/// The interactive application state.
struct App {
    session: Arc<Session>,
    namespaces: Arc<NamespaceTable>,
    filters: Arc<FilterSet>,
    options: TuiOptions,
    arena: Vec<Node>,
    roots: Vec<usize>,
    visible: Vec<usize>,
    selected: usize,
    list_state: ListState,
    status: String,
    filter_query: String,
    filter_mode: bool,
    /// When true, the `?` help overlay is shown and captures keys.
    help_open: bool,
    /// First visible line of the help overlay (when it is taller than the screen).
    help_scroll: u16,
    /// When true, an acquire-config prompt is capturing "<mode> <interval>".
    acquire_mode: bool,
    /// Current text of the acquire-config prompt.
    acquire_query: String,
    /// What the running background node collection is for.
    collect_pending: Option<CollectFor>,
    /// When true, hide nodes that don't lead to Variables (declutter view).
    vars_only: bool,
    /// In-progress background job state (for the progress bar), if any.
    job: Option<JobState>,
    /// Cancel flag for the running background job (e.g. export).
    job_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Receiver handed to the event loop when a job is launched.
    pending_job: Option<mpsc::Receiver<JobMsg>>,
    /// Spinner animation frame counter.
    spinner: usize,
    /// Number of pre-selected nodes (from a loaded config) located so far.
    located: usize,
    quit: bool,
}

impl App {
    fn new(
        session: Arc<Session>,
        namespaces: Arc<NamespaceTable>,
        filters: Arc<FilterSet>,
        options: TuiOptions,
    ) -> Self {
        let status = if let Some(path) = &options.source_config {
            format!(
                "editing {} · selection 0/{} located · a saves the selection",
                path.display(),
                options.selected_ids.len()
            )
        } else {
            "ready — press ? for all keys".into()
        };
        Self {
            session,
            namespaces,
            filters,
            options,
            arena: Vec::new(),
            roots: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            status,
            filter_query: String::new(),
            filter_mode: false,
            help_open: false,
            help_scroll: 0,
            acquire_mode: false,
            acquire_query: String::new(),
            collect_pending: None,
            vars_only: false,
            job: None,
            job_cancel: None,
            pending_job: None,
            spinner: 0,
            located: 0,
            quit: false,
        }
    }

    /// Read the root node and load its top-level children.
    async fn bootstrap(&mut self) {
        let root_id = self.options.root.clone();
        let label = self
            .read_label(&root_id)
            .await
            .unwrap_or_else(|| "Root".into());
        let marked = self.options.selected_ids.contains(&root_id.to_string());
        let root = Node {
            node_id: root_id,
            label,
            class: NodeClass::Object,
            path: String::new(),
            depth: 0,
            parent: None,
            children: Vec::new(),
            loaded: false,
            expanded: true,
            expandable: true,
            loading: false,
            marked,
            mode: NodeMode::Auto,
            attrs: None,
            value: None,
        };
        self.arena.push(root);
        self.roots.push(0);
        if marked {
            self.note_located();
        }
        self.load_children(0).await;
        self.rebuild_visible();
    }

    /// Best-effort display name for a node id.
    async fn read_label(&self, node_id: &NodeId) -> Option<String> {
        let attrs = read_node_attributes(&self.session, std::slice::from_ref(node_id))
            .await
            .ok()?;
        attrs
            .into_iter()
            .next()
            .and_then(|a| a.display_name)
            .filter(|s| !s.is_empty())
    }

    /// Browse and attach the children of `parent_idx` synchronously. Used for
    /// the initial root load during bootstrap; interactive expansion uses the
    /// non-blocking [`App::start_browse_job`] instead.
    async fn load_children(&mut self, parent_idx: usize) {
        if self.arena[parent_idx].loaded {
            return;
        }
        let parent_id = self.arena[parent_idx].node_id.clone();
        let parent_path = self.arena[parent_idx].path.clone();

        match browse_into_children(
            &self.session,
            &self.filters,
            &parent_id,
            &parent_path,
            self.options.page_size,
        )
        .await
        {
            Ok(children) => self.attach_children(parent_idx, children),
            Err(e) => {
                self.status = format!("browse failed: {e}");
                self.arena[parent_idx].loaded = true;
                self.arena[parent_idx].loading = false;
            }
        }
    }

    /// Insert already-browsed children under `parent_idx`.
    fn attach_children(&mut self, parent_idx: usize, children: Vec<LoadedChild>) {
        let parent_depth = self.arena[parent_idx].depth;
        let mut child_indices = Vec::with_capacity(children.len());
        let mut newly_located = 0;
        for child in children {
            let marked = self
                .options
                .selected_ids
                .contains(&child.node_id.to_string());
            if marked {
                newly_located += 1;
            }
            let node = Node {
                node_id: child.node_id,
                label: child.label,
                class: child.class,
                path: child.path,
                depth: parent_depth + 1,
                parent: Some(parent_idx),
                children: Vec::new(),
                loaded: false,
                expanded: false,
                expandable: child.expandable,
                loading: false,
                marked,
                mode: NodeMode::Auto,
                attrs: child.attrs,
                value: None,
            };
            let idx = self.arena.len();
            self.arena.push(node);
            child_indices.push(idx);
        }
        self.arena[parent_idx].children = child_indices;
        self.arena[parent_idx].loaded = true;
        self.arena[parent_idx].loading = false;
        for _ in 0..newly_located {
            self.note_located();
        }
    }

    /// Record that a pre-selected node was found in the tree and refresh the
    /// "M/N located" status line when editing a loaded config.
    fn note_located(&mut self) {
        self.located += 1;
        if let Some(path) = &self.options.source_config {
            self.status = format!(
                "editing {} · selection {}/{} located · a saves the selection",
                path.display(),
                self.located,
                self.options.selected_ids.len()
            );
        }
    }

    /// Reveal pre-selected nodes loaded from a config by browsing down to each
    /// one and expanding its ancestors, so they appear marked (`[x]`) on startup
    /// instead of staying hidden behind collapsed, never-loaded branches.
    ///
    /// The crawl is bounded to the branches that actually lead to a selected
    /// node: a node is only descended into when its browse path is a prefix of
    /// some selected node's saved path. Nodes are marked by id in
    /// [`App::attach_children`], so legacy ids without a saved path are still
    /// matched as long as they fall on a revealed branch.
    async fn locate_selected(&mut self) {
        if self.options.selected_ids.is_empty() || self.options.selected_paths.is_empty() {
            return;
        }

        // Every path prefix that leads to a selected node, e.g. the selection
        // "FCX001/Measurement" contributes {"FCX001", "FCX001/Measurement"}.
        let mut prefixes: std::collections::HashSet<String> = std::collections::HashSet::new();
        for full in &self.options.selected_paths {
            let mut acc = String::new();
            for segment in full.split('/') {
                if acc.is_empty() {
                    acc.push_str(segment);
                } else {
                    acc.push('/');
                    acc.push_str(segment);
                }
                prefixes.insert(acc.clone());
            }
        }

        // Breadth-first descent, loading children only along needed branches.
        let mut queue: std::collections::VecDeque<usize> = self.roots.iter().copied().collect();
        while let Some(idx) = queue.pop_front() {
            let path = self.arena[idx].path.clone();
            // The root (empty path) is always descended; otherwise only branches
            // that lead to a selected node.
            if !path.is_empty() && !prefixes.contains(&path) {
                continue;
            }
            if !self.arena[idx].loaded {
                self.load_children(idx).await;
            }
            self.arena[idx].expanded = true;
            for child in self.arena[idx].children.clone() {
                queue.push_back(child);
            }
        }

        // Park the cursor on the first revealed selection for convenience.
        self.rebuild_visible();
        if let Some(pos) = self.visible.iter().position(|&idx| self.arena[idx].marked) {
            self.selected = pos;
            self.list_state.select(Some(pos));
        }
    }

    /// Spawn a background browse so the UI stays responsive on slow servers or
    /// large nodes. The result is applied when the job reports back.
    fn start_browse_job(&mut self, idx: usize) {
        let session = self.session.clone();
        let filters = self.filters.clone();
        let parent_id = self.arena[idx].node_id.clone();
        let parent_path = self.arena[idx].path.clone();
        let page_size = self.options.page_size;
        let (tx, rx) = mpsc::channel(4);

        self.arena[idx].loading = true;
        self.job = Some(JobState {
            verb: format!("Browsing {}", self.arena[idx].label),
            processed: 0,
            counted: false,
        });
        self.pending_job = Some(rx);
        self.status = format!("browsing {}…", self.arena[idx].label);

        tokio::spawn(async move {
            let result =
                browse_into_children(&session, &filters, &parent_id, &parent_path, page_size).await;
            let _ = tx
                .send(JobMsg::BrowseDone {
                    parent: idx,
                    result,
                })
                .await;
        });
    }

    /// Recompute the flattened list of visible node indices.
    fn rebuild_visible(&mut self) {
        let mut visible = Vec::new();
        for &root in &self.roots {
            collect_visible(&self.arena, root, &mut visible);
        }

        if self.vars_only {
            let keep = self.compute_vars_keep();
            visible.retain(|&idx| keep[idx]);
        }

        if !self.filter_query.is_empty() {
            let q = self.filter_query.to_lowercase();
            visible.retain(|&idx| self.arena[idx].path.to_lowercase().contains(&q));
        }

        self.visible = visible;
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
        self.list_state.select(if self.visible.is_empty() {
            None
        } else {
            Some(self.selected)
        });
    }

    /// Currently selected arena node, if any.
    fn selected_idx(&self) -> Option<usize> {
        self.visible.get(self.selected).copied()
    }

    /// Lazily load attributes for the selected node so the detail pane is filled.
    async fn ensure_selected_detail(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if self.arena[idx].attrs.is_some() {
            return;
        }
        let node_id = self.arena[idx].node_id.clone();
        if let Ok(attrs) = read_node_attributes(&self.session, std::slice::from_ref(&node_id)).await
        {
            self.arena[idx].attrs = attrs.into_iter().next();
        }
    }

    /// Handle a key press.
    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.help_open {
            match key.code {
                KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc => self.help_open = false,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.help_scroll = self.help_scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.quit = true;
                }
                _ => {}
            }
            return Ok(());
        }
        if self.filter_mode {
            self.handle_filter_key(key);
            return Ok(());
        }
        if self.acquire_mode {
            self.handle_acquire_key(key);
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('?') => {
                self.help_open = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('g') => {
                self.selected = 0;
                self.rebuild_visible();
            }
            KeyCode::Char('G') => {
                self.selected = self.visible.len().saturating_sub(1);
                self.rebuild_visible();
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.expand_selected().await,
            KeyCode::Left | KeyCode::Char('h') => self.collapse_selected(),
            KeyCode::Char(' ') => self.toggle_mark_selected(),
            KeyCode::Char('S') => self.mark_mode_selected(GenMode::Subscribe),
            KeyCode::Char('P') => self.mark_mode_selected(GenMode::Poll),
            KeyCode::Char('V') => self.toggle_vars_only(),
            KeyCode::Char('v') => self.preview_value().await,
            KeyCode::Char('s') => self.validate_selected().await,
            KeyCode::Char('e') => self.export(),
            KeyCode::Char('a') => {
                if self.options.source_config.is_some() {
                    // Editing an existing config: update its selection directly,
                    // no mode/interval prompt (the tasks are preserved as-is).
                    self.start_acquire("poll 1s");
                } else {
                    self.acquire_mode = true;
                    self.acquire_query = "poll 1s".into();
                    self.status =
                        "acquire: '<poll|subscribe> <interval>' (Enter write config, Esc cancel)"
                            .into();
                }
            }
            KeyCode::Char('t') => self.start_library(),
            KeyCode::Char('x') => self.cancel_job(),
            KeyCode::Char('/') => {
                self.filter_mode = true;
                self.filter_query.clear();
                self.status = "filter: (type to filter, Enter apply, Esc clear)".into();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter_mode = false;
                self.filter_query.clear();
                self.rebuild_visible();
                self.status = "filter cleared".into();
            }
            KeyCode::Enter => {
                self.filter_mode = false;
                self.status = format!("filter: '{}'", self.filter_query);
                self.rebuild_visible();
            }
            KeyCode::Backspace => {
                self.filter_query.pop();
                self.rebuild_visible();
            }
            KeyCode::Char(c) => {
                self.filter_query.push(c);
                self.rebuild_visible();
            }
            _ => {}
        }
    }

    fn handle_acquire_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.acquire_mode = false;
                self.acquire_query.clear();
                self.status = "acquire cancelled".into();
            }
            KeyCode::Enter => {
                self.acquire_mode = false;
                let query = std::mem::take(&mut self.acquire_query);
                self.start_acquire(&query);
            }
            KeyCode::Backspace => {
                self.acquire_query.pop();
            }
            KeyCode::Char(c) => {
                self.acquire_query.push(c);
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: i64) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as i64;
        let next = (self.selected as i64 + delta).clamp(0, len - 1);
        self.selected = next as usize;
        self.list_state.select(Some(self.selected));
    }

    async fn expand_selected(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if !self.arena[idx].expandable {
            self.status = format!("{} is a leaf node", self.arena[idx].label);
            return;
        }
        if self.arena[idx].loaded {
            // Already browsed: just reveal the children (cheap, instant).
            self.arena[idx].expanded = true;
            self.rebuild_visible();
            self.status = format!(
                "{} ({} children)",
                self.arena[idx].label,
                self.arena[idx].children.len()
            );
            return;
        }
        if self.job.is_some() || self.arena[idx].loading {
            self.status = "busy — please wait for the current operation".into();
            return;
        }
        // Browse in the background so the UI never freezes on slow servers.
        self.start_browse_job(idx);
    }

    /// Apply a message from the background job channel. Returns `true` when the
    /// job has finished and its receiver should be dropped.
    fn handle_job_msg(&mut self, msg: JobMsg) -> bool {
        match msg {
            JobMsg::Progress { processed } => {
                if let Some(job) = &mut self.job {
                    job.processed = processed;
                    job.counted = true;
                }
                false
            }
            JobMsg::BrowseDone { parent, result } => {
                match result {
                    Ok(children) => {
                        let n = children.len();
                        self.attach_children(parent, children);
                        self.arena[parent].expanded = true;
                        self.status = format!("{} ({n} children)", self.arena[parent].label);
                    }
                    Err(e) => {
                        self.arena[parent].loaded = true;
                        self.arena[parent].loading = false;
                        self.status = format!("browse failed: {e}");
                    }
                }
                self.job = None;
                self.rebuild_visible();
                if let Some(pos) = self.visible.iter().position(|&v| v == parent) {
                    self.selected = pos;
                    self.list_state.select(Some(pos));
                }
                true
            }
            JobMsg::ExportDone { result, path } => {
                self.status = match result {
                    Ok(n) => format!("exported {n} nodes to {}", path.display()),
                    Err(e) => format!("export failed: {e}"),
                };
                self.job = None;
                self.job_cancel = None;
                true
            }
            JobMsg::LibraryReady {
                records,
                schema,
                remove,
            } => {
                self.job = None;
                self.job_cancel = None;
                self.write_library(&records, &remove, &schema);
                true
            }
            JobMsg::CollectDone { result } => {
                self.job = None;
                self.job_cancel = None;
                let pending = self.collect_pending.take();
                match (result, pending) {
                    (Ok(records), Some(CollectFor::Acquire(mode, interval))) => {
                        let nodes: Vec<_> = records
                            .iter()
                            .filter(|(r, _)| {
                                r.node_class == "Variable" && !record_is_unreadable_variable(r)
                            })
                            .map(|(r, m)| record_to_gennode(r, *m))
                            .collect();
                        if nodes.is_empty() {
                            self.status = "no variables found in the marked selection".into();
                        } else {
                            self.write_acquire_config(nodes, mode, interval);
                        }
                    }
                    (Ok(records), Some(CollectFor::Library { remove })) => {
                        let records: Vec<NodeRecord> =
                            records.into_iter().map(|(r, _)| r).collect();
                        self.start_discovery(records, remove);
                    }
                    (Ok(_), None) => {
                        self.status = "collect: internal error (nothing pending)".into();
                    }
                    (Err(e), _) => self.status = format!("collect failed: {e}"),
                }
                true
            }
        }
    }

    fn collapse_selected(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if self.arena[idx].expanded && !self.arena[idx].children.is_empty() {
            self.arena[idx].expanded = false;
        } else if let Some(parent) = self.arena[idx].parent {
            // Move selection to the parent and collapse it.
            self.arena[parent].expanded = false;
            self.rebuild_visible();
            if let Some(pos) = self.visible.iter().position(|&v| v == parent) {
                self.selected = pos;
            }
        }
        self.rebuild_visible();
    }

    /// Toggle the export mark on the selected node. Marking is *logical* and
    /// instant: the node and its whole subtree are considered marked, but the
    /// children are only traversed lazily when the export job runs. This keeps
    /// the UI responsive even for huge subtrees.
    fn toggle_mark_selected(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        let covered = self.ancestor_marked(idx);
        let now_marked = !self.arena[idx].marked;
        self.arena[idx].marked = now_marked;
        self.arena[idx].mode = NodeMode::Auto;
        let n = self.marked_count();
        self.status = if now_marked {
            if covered {
                format!(
                    "marked {} (already inside a marked parent) · {n} subtree(s) marked",
                    self.arena[idx].label
                )
            } else {
                format!(
                    "marked {} and all its children · {n} subtree(s) marked",
                    self.arena[idx].label
                )
            }
        } else {
            format!("unmarked {} · {n} subtree(s) marked", self.arena[idx].label)
        };
    }

    /// Mark the selected node (and its subtree) with an explicit acquisition
    /// mode. Pressing the same mode again on an already-marked node unmarks it;
    /// pressing a different mode switches it.
    fn mark_mode_selected(&mut self, mode: GenMode) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        let target = match mode {
            GenMode::Subscribe => NodeMode::Subscribe,
            GenMode::Poll => NodeMode::Poll,
        };
        let label = self.arena[idx].label.clone();
        if self.arena[idx].marked && self.arena[idx].mode == target {
            self.arena[idx].marked = false;
            self.arena[idx].mode = NodeMode::Auto;
            let n = self.marked_count();
            self.status = format!("unmarked {label} · {n} subtree(s) marked");
            return;
        }
        let covered = self.ancestor_marked(idx);
        self.arena[idx].marked = true;
        self.arena[idx].mode = target;
        let n = self.marked_count();
        let verb = match mode {
            GenMode::Subscribe => "subscribe",
            GenMode::Poll => "poll",
        };
        self.status = if covered {
            format!("marked {label} as {verb} (inside a marked parent) · {n} subtree(s) marked")
        } else {
            format!("marked {label} and its children as {verb} · {n} subtree(s) marked")
        };
    }

    /// Whether any *ancestor* of `idx` is explicitly marked.
    fn ancestor_marked(&self, idx: usize) -> bool {
        let mut cur = self.arena[idx].parent;
        while let Some(i) = cur {
            if self.arena[i].marked {
                return true;
            }
            cur = self.arena[i].parent;
        }
        false
    }

    /// The mode of the nearest marked ancestor of `idx`, if any.
    fn ancestor_mark_mode(&self, idx: usize) -> Option<NodeMode> {
        let mut cur = self.arena[idx].parent;
        while let Some(i) = cur {
            if self.arena[i].marked {
                return Some(self.arena[i].mode);
            }
            cur = self.arena[i].parent;
        }
        None
    }

    /// Toggle hiding of nodes that don't lead to Variables.
    fn toggle_vars_only(&mut self) {
        self.vars_only = !self.vars_only;
        // Keep the selection valid against the new visible set.
        let current = self.selected_idx();
        self.rebuild_visible();
        if let Some(idx) = current
            && let Some(pos) = self.visible.iter().position(|&v| v == idx)
        {
            self.selected = pos;
            self.list_state.select(Some(pos));
        }
        self.status = if self.vars_only {
            "variables-only view (V to show all)".into()
        } else {
            "showing all nodes".into()
        };
    }

    /// Number of subtrees explicitly marked for export (top-level marks only).
    fn marked_count(&self) -> usize {
        self.arena
            .iter()
            .enumerate()
            .filter(|(i, n)| n.marked && n.parent.is_some() && !self.ancestor_marked(*i))
            .count()
    }

    /// Compute, for each arena node, whether it should remain visible in the
    /// variables-only view: Variables and any container leading to a Variable
    /// (plus not-yet-loaded containers so the user can still drill in).
    fn compute_vars_keep(&self) -> Vec<bool> {
        let mut keep = vec![false; self.arena.len()];
        for &root in &self.roots {
            self.mark_vars_keep(root, &mut keep);
        }
        keep
    }

    fn mark_vars_keep(&self, idx: usize, keep: &mut [bool]) -> bool {
        let node = &self.arena[idx];
        let mut any = node.class == NodeClass::Variable;
        for &child in &node.children {
            if self.mark_vars_keep(child, keep) {
                any = true;
            }
        }
        // Keep unexplored containers so variables hidden behind them stay reachable.
        if !node.loaded && is_expandable(node.class) && node.class != NodeClass::Variable {
            any = true;
        }
        keep[idx] = any;
        any
    }

    async fn preview_value(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if self.arena[idx].class != NodeClass::Variable {
            self.status = "value preview only applies to Variables".into();
            return;
        }
        let node_id = self.arena[idx].node_id.clone();
        match read_values(&self.session, std::slice::from_ref(&node_id)).await {
            Ok(values) => {
                let value = values.into_iter().next().and_then(|v| v.value);
                self.arena[idx].value = value.clone();
                self.status = match value {
                    Some(v) => format!("value = {v}"),
                    None => "value = (none)".into(),
                };
            }
            Err(e) => self.status = format!("read failed: {e}"),
        }
    }

    async fn validate_selected(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if self.arena[idx].class != NodeClass::Variable {
            self.status = "validate only applies to Variables".into();
            return;
        }
        let node_id = self.arena[idx].node_id.clone();
        self.status = "validating subscription…".into();
        match subscription::validate_one(&self.session, &node_id).await {
            Ok(()) => self.status = "subscription OK".into(),
            Err(e) => self.status = format!("not subscribable: {e}"),
        }
    }

    fn export(&mut self) {
        if self.job.is_some() {
            self.status = "busy — please wait for the current operation".into();
            return;
        }

        let path = self
            .options
            .export_file
            .clone()
            .unwrap_or_else(|| PathBuf::from("opcua-scan-export.json"));
        let encoding = encoding_for(&path);

        // Top-level marked nodes (a mark covers its whole subtree).
        let marked_roots: Vec<usize> = self
            .arena
            .iter()
            .enumerate()
            .filter(|(i, n)| n.marked && n.parent.is_some() && !self.ancestor_marked(*i))
            .map(|(i, _)| i)
            .collect();

        if marked_roots.is_empty() {
            // Nothing marked: export whatever is already loaded (in-memory, fast).
            let mut records: Vec<NodeRecord> = self
                .arena
                .iter()
                .filter(|n| n.parent.is_some())
                .map(|n| self.node_to_record(n))
                .collect();
            if records.is_empty() {
                self.status = "nothing marked or loaded to export yet".into();
                return;
            }
            self.status = match write_export(&mut records, encoding, Some(&path)) {
                Ok(()) => format!(
                    "exported {} loaded nodes to {}",
                    records.len(),
                    path.display()
                ),
                Err(e) => format!("export failed: {e}"),
            };
            return;
        }

        // Concurrent background export of each marked subtree (like the CLI scan).
        let roots: Vec<ExportRoot> = marked_roots
            .iter()
            .map(|&idx| ExportRoot {
                node_id: self.arena[idx].node_id.clone(),
                path: self.arena[idx].path.clone(),
                record: self.node_to_record(&self.arena[idx]),
                mode: self.arena[idx].mode,
            })
            .collect();

        let session = self.session.clone();
        let filters = self.filters.clone();
        let namespaces = self.namespaces.clone();
        let tmpl = ExportTemplate {
            strategy: self.options.strategy,
            workers: self.options.workers,
            page_size: self.options.page_size,
            max_children: self.options.max_children,
            retry: self.options.retry,
            preview_values: self.options.preview_values,
        };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = mpsc::channel(16);

        self.job = Some(JobState {
            verb: format!("Exporting {} subtree(s)", roots.len()),
            processed: 0,
            counted: true,
        });
        self.job_cancel = Some(cancel.clone());
        self.pending_job = Some(rx);
        self.status = "exporting in background… (x to cancel)".into();

        tokio::spawn(async move {
            export_job(
                session, filters, namespaces, roots, tmpl, path, encoding, cancel, tx,
            )
            .await;
        });
    }

    /// Request cancellation of the running background job, if any.
    fn cancel_job(&mut self) {
        if let Some(cancel) = &self.job_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.status = "cancelling…".into();
        }
    }

    /// Parse the acquire prompt ("<poll|subscribe> <interval>"), collect the
    /// selected Variable node ids, and write an `acquire` config. Marked
    /// subtrees are traversed in the background; with nothing marked, the
    /// already-loaded Variables are used immediately.
    fn start_acquire(&mut self, query: &str) {
        if self.job.is_some() {
            self.status = "busy — please wait for the current operation".into();
            return;
        }

        let (mode, interval) = match parse_acquire_query(query) {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("acquire: {e}");
                return;
            }
        };

        let marked_roots = self.marked_roots();
        if marked_roots.is_empty() {
            // Nothing marked: use the Variables already loaded in memory.
            let candidates: Vec<&Node> = self
                .arena
                .iter()
                .filter(|n| n.parent.is_some() && n.class == NodeClass::Variable)
                .collect();
            let skipped = candidates
                .iter()
                .filter(|n| node_is_unreadable_variable(n))
                .count();
            let nodes: Vec<crate::acquire::generate::GenNode> = candidates
                .iter()
                .filter(|n| !node_is_unreadable_variable(n))
                .map(|n| self.node_to_gennode(n))
                .collect();
            if nodes.is_empty() {
                self.status = if skipped > 0 {
                    format!(
                        "no readable variables (skipped {skipped} non-readable) — mark nodes with space, then press a"
                    )
                } else {
                    "no variables marked or loaded — mark nodes with space, then press a".into()
                };
                return;
            }
            self.write_acquire_config(nodes, mode, interval);
            return;
        }

        self.start_collect(&marked_roots, CollectFor::Acquire(mode, interval));
    }

    /// Top-level marked subtrees (a mark covers the whole subtree).
    fn marked_roots(&self) -> Vec<usize> {
        self.arena
            .iter()
            .enumerate()
            .filter(|(i, n)| n.marked && n.parent.is_some() && !self.ancestor_marked(*i))
            .map(|(i, _)| i)
            .collect()
    }

    /// Traverse each marked subtree in the background to collect Variables;
    /// `pending` says what to do with them when the job reports back.
    fn start_collect(&mut self, marked_roots: &[usize], pending: CollectFor) {
        let roots: Vec<ExportRoot> = marked_roots
            .iter()
            .map(|&idx| ExportRoot {
                node_id: self.arena[idx].node_id.clone(),
                path: self.arena[idx].path.clone(),
                record: self.node_to_record(&self.arena[idx]),
                mode: self.arena[idx].mode,
            })
            .collect();

        let session = self.session.clone();
        let filters = self.filters.clone();
        let namespaces = self.namespaces.clone();
        let tmpl = ExportTemplate {
            strategy: self.options.strategy,
            workers: self.options.workers,
            page_size: self.options.page_size,
            max_children: self.options.max_children,
            retry: self.options.retry,
            preview_values: false,
        };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = mpsc::channel(16);

        self.collect_pending = Some(pending);
        self.job = Some(JobState {
            verb: format!("Collecting variables from {} subtree(s)", roots.len()),
            processed: 0,
            counted: true,
        });
        self.job_cancel = Some(cancel.clone());
        self.pending_job = Some(rx);
        self.status = "collecting variables in background… (x to cancel)".into();

        tokio::spawn(async move {
            collect_job(session, filters, namespaces, roots, tmpl, cancel, tx).await;
        });
    }

    /// Write the marked nodes as a tedge-dot point library (`t`). Marked
    /// subtrees are traversed in the background; with nothing marked, the
    /// already-loaded Variables are used immediately.
    fn start_library(&mut self) {
        if self.job.is_some() {
            self.status = "busy — please wait for the current operation".into();
            return;
        }
        let marked_roots = self.marked_roots();
        if marked_roots.is_empty() {
            let records: Vec<NodeRecord> = self
                .arena
                .iter()
                .filter(|n| n.parent.is_some() && is_point_class(n.class))
                .map(|n| self.node_to_record(n))
                .collect();
            if records.is_empty() {
                self.status =
                    "no variables or methods marked or loaded — mark nodes with space, then press t"
                        .into();
                return;
            }
            self.start_discovery(records, Default::default());
            return;
        }
        // A library point is removed when its node is shown in the tree and
        // left unmarked; nodes never loaded keep their points.
        let remove = self
            .arena
            .iter()
            .enumerate()
            .filter(|(i, n)| {
                n.parent.is_some()
                    && is_point_class(n.class)
                    && !n.marked
                    && !self.ancestor_marked(*i)
            })
            .map(|(_, n)| n.node_id.to_string())
            .collect();
        self.start_collect(&marked_roots, CollectFor::Library { remove });
    }

    /// Discover the custom datatypes and method arguments of `records` in the
    /// background; the library is written when the job reports back.
    fn start_discovery(
        &mut self,
        records: Vec<NodeRecord>,
        remove: std::collections::HashSet<String>,
    ) {
        let session = self.session.clone();
        let (tx, rx) = mpsc::channel(1);
        self.job = Some(JobState {
            verb: "Reading datatypes and method arguments".into(),
            processed: 0,
            counted: false,
        });
        self.pending_job = Some(rx);
        self.status = "reading datatypes and method arguments…".into();
        tokio::spawn(async move {
            let schema = crate::tedge_dot::schema::discover(&session, &records).await;
            let _ = tx
                .send(JobMsg::LibraryReady {
                    records,
                    schema,
                    remove,
                })
                .await;
        });
    }

    /// Write (or merge into) the point library and report the counts.
    fn write_library(
        &mut self,
        records: &[NodeRecord],
        remove: &std::collections::HashSet<String>,
        schema: &crate::tedge_dot::schema::Schema,
    ) {
        let path = self.options.library_file.clone();
        let existing = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(text) => Some(text),
                Err(e) => {
                    self.status = format!("library read failed: {e}");
                    return;
                }
            }
        } else {
            None
        };
        self.status = match crate::tedge_dot::library::write(
            existing.as_deref(),
            records,
            remove,
            &self.namespaces,
            &self.options.library_options,
            schema,
        ) {
            Ok(outcome) => match std::fs::write(&path, &outcome.text) {
                Ok(()) => format!(
                    "library {}: +{} −{} point(s), {} in total, {} node(s) skipped",
                    path.display(),
                    outcome.added,
                    outcome.removed,
                    outcome.total,
                    outcome.skipped.len()
                ),
                Err(e) => format!("library write failed: {e}"),
            },
            Err(e) => format!("library {}: {e}", path.display()),
        };
    }

    /// Generate and write the acquire config to the configured path.
    fn write_acquire_config(
        &mut self,
        nodes: Vec<crate::acquire::generate::GenNode>,
        mode: crate::acquire::generate::GenMode,
        interval: Duration,
    ) {
        let path = self.options.acquire_file.clone();
        let count = nodes.len();

        // Editing an existing config: update only the selection in place,
        // preserving connection, tasks, output, comments and formatting.
        if let Some(src) = self.options.source_config.clone() {
            match std::fs::read_to_string(&src) {
                Ok(original) => {
                    match crate::acquire::generate::update_selection_in_toml(&original, &nodes) {
                        Ok(updated) => match std::fs::write(&path, updated) {
                            Ok(()) => {
                                self.status =
                                    format!("updated selection ({count}) in {}", path.display());
                            }
                            Err(e) => self.status = format!("acquire write failed: {e}"),
                        },
                        Err(e) => self.status = format!("acquire update failed: {e}"),
                    }
                }
                Err(e) => self.status = format!("acquire read failed: {e}"),
            }
            return;
        }

        match crate::acquire::generate::generate_toml(
            &self.options.connection,
            nodes,
            mode,
            interval,
        ) {
            Ok(toml) => match std::fs::write(&path, toml) {
                Ok(()) => {
                    self.status = format!(
                        "wrote {count} node(s) to {} — run: opcua-scan acquire --config {}",
                        path.display(),
                        path.display()
                    );
                }
                Err(e) => self.status = format!("acquire write failed: {e}"),
            },
            Err(e) => self.status = format!("acquire config failed: {e}"),
        }
    }

    fn node_to_record(&self, node: &Node) -> NodeRecord {
        let attrs = node.attrs.clone().unwrap_or_default();
        let datatype = attrs.data_type.as_ref().map(|dt| {
            crate::filters::predicate::builtin_datatype_name(dt)
                .map(|s| s.to_string())
                .unwrap_or_else(|| dt.to_string())
        });
        NodeRecord {
            browse_path: node.path.clone(),
            display_name: attrs
                .display_name
                .clone()
                .unwrap_or_else(|| node.label.clone()),
            browse_name: node.label.clone(),
            node_id: node.node_id.to_string(),
            namespace_uri: self.namespaces.display(node.node_id.namespace),
            node_class: node_class_name(node.class).to_string(),
            datatype,
            value_rank: attrs.value_rank,
            access_level: attrs.access_level,
            user_access_level: attrs.user_access_level,
            historizing: attrs.historizing,
            subscribable: is_subscribable(&attrs),
            validation_status: None,
            current_value: node.value.clone(),
            engineering_units: None,
            description: attrs.description.clone(),
            parent_node_id: node
                .parent
                .filter(|_| node.class == NodeClass::Method)
                .map(|p| self.arena[p].node_id.to_string()),
        }
    }

    /// Build a [`GenNode`] (for an acquire config) from a tree node, capturing
    /// the namespace URI so the id can be remapped if the server later reorders
    /// its namespace array.
    fn node_to_gennode(&self, node: &Node) -> crate::acquire::generate::GenNode {
        let namespace = match self.namespaces.uri(node.node_id.namespace) {
            Some(uri) if node.node_id.namespace != 0 && !uri.is_empty() => Some(uri.to_string()),
            _ => None,
        };
        let name = node
            .attrs
            .as_ref()
            .and_then(|a| a.display_name.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(node.label.clone()).filter(|s| !s.is_empty()));
        crate::acquire::generate::GenNode {
            id: node.node_id.to_string(),
            namespace,
            name,
            path: Some(node.path.clone()).filter(|s| !s.is_empty()),
            mode: gen_mode_of(node.mode),
        }
    }

    /// Render the whole UI.
    fn render(&mut self, f: &mut Frame) {
        // Panes, key legend, a progress row only while a job runs, status.
        let mut constraints = vec![Constraint::Min(1), Constraint::Length(1)];
        if self.job.is_some() {
            constraints.push(Constraint::Length(1));
        }
        constraints.push(Constraint::Length(1));
        let outer = Layout::vertical(constraints).split(f.area());
        let panes = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(outer[0]);

        self.render_tree(f, panes[0]);
        self.render_detail(f, panes[1]);
        self.render_legend(f, outer[1]);
        if self.job.is_some() {
            self.render_progress(f, outer[2]);
            self.render_footer(f, outer[3]);
        } else {
            self.render_footer(f, outer[2]);
        }
        if self.help_open {
            self.help_scroll = render_help(f, self.help_scroll);
        }
    }

    /// The key legend entries of the current context, with priorities
    /// (lower is more important), and whether `? help` is appended.
    fn legend_entries(&self) -> (Vec<LegendEntry>, bool) {
        if self.help_open {
            return (vec![("?/Esc", "close help", 0), ("↑↓", "scroll", 1)], false);
        }
        if self.filter_mode {
            return (
                vec![
                    ("type", "filter text", 0),
                    ("⏎", "apply", 1),
                    ("Esc", "clear", 2),
                    ("⌫", "delete", 3),
                ],
                false,
            );
        }
        if self.acquire_mode {
            return (
                vec![
                    ("type", "poll 1s | subscribe 500ms", 0),
                    ("⏎", "write config", 1),
                    ("Esc", "cancel", 2),
                ],
                false,
            );
        }
        let mut entries: Vec<LegendEntry> = Vec::new();
        if self.job.is_some() {
            entries.push(("x", "cancel", 0));
        }
        entries.extend(KEY_HELP.iter().filter_map(|k| k.legend));
        (entries, true)
    }

    /// The legend row: `key action` pairs that fit the width.
    fn render_legend(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let (entries, with_help) = self.legend_entries();
        let kept = fit_legend(&entries, with_help, area.width as usize);
        let mut spans: Vec<Span> = Vec::new();
        for (i, (key, label)) in kept.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(
                *key,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(" {label}"),
                Style::default().fg(Color::Gray),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Render an animated progress bar for the active background job.
    fn render_progress(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let Some(job) = &self.job else {
            return;
        };
        let spinner = SPINNER[self.spinner % SPINNER.len()];
        // Total is unknown for streaming work, so animate a "throbber" sweep and
        // surface the live processed count in the label instead of a percentage.
        let ratio = ((self.spinner % 20) as f64) / 19.0;
        let ratio = if ratio > 1.0 { 2.0 - ratio } else { ratio };
        let label = if job.counted {
            format!(
                "{spinner} {} · {} nodes · x to cancel",
                job.verb, job.processed
            )
        } else {
            format!("{spinner} {}…", job.verb)
        };
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Cyan).bg(Color::Black))
            .ratio(ratio.clamp(0.0, 1.0))
            .label(label);
        f.render_widget(gauge, area);
    }

    fn render_tree(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let items: Vec<ListItem> = self
            .visible
            .iter()
            .map(|&idx| {
                let node = &self.arena[idx];
                let indent = "  ".repeat(node.depth.saturating_sub(1));
                let marker = if node.expandable {
                    if node.expanded { "▼ " } else { "▶ " }
                } else {
                    "  "
                };
                let (check, check_style) = if self.arena[idx].marked {
                    match self.arena[idx].mode {
                        NodeMode::Subscribe => ("[S] ", Style::default().fg(Color::Cyan)),
                        NodeMode::Poll => ("[P] ", Style::default().fg(Color::Yellow)),
                        NodeMode::Auto => ("[x] ", Style::default().fg(Color::Green)),
                    }
                } else if let Some(mode) = self.ancestor_mark_mode(idx) {
                    // Covered by a marked ancestor: show a lighter tick tinted
                    // by the ancestor's mode.
                    let color = match mode {
                        NodeMode::Subscribe => Color::Cyan,
                        NodeMode::Poll => Color::Yellow,
                        NodeMode::Auto => Color::Green,
                    };
                    ("[+] ", Style::default().fg(color))
                } else {
                    ("[ ] ", Style::default().fg(Color::DarkGray))
                };
                let class = short_class(node.class);
                let line = Line::from(vec![
                    Span::styled(check, check_style),
                    Span::raw(format!("{indent}{marker}")),
                    Span::styled(node.label.clone(), Style::default().fg(Color::White)),
                    Span::styled(format!("  {class}"), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let marked = self.marked_count();
        let mut title = if self.filter_query.is_empty() {
            "Address space".to_string()
        } else {
            format!("Address space  [filter: {}]", self.filter_query)
        };
        if self.vars_only {
            title.push_str("  [vars-only]");
        }
        if marked > 0 {
            title.push_str(&format!("  [{marked} marked]"));
        }
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");
        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_detail(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let mut lines: Vec<Line> = Vec::new();
        if let Some(idx) = self.selected_idx() {
            let node = &self.arena[idx];
            lines.push(detail_line("Browse name", &node.label));
            lines.push(detail_line("Browse path", &node.path));
            lines.push(detail_line("Node id", &node.node_id.to_string()));
            lines.push(detail_line(
                "Namespace",
                &self.namespaces.display(node.node_id.namespace),
            ));
            lines.push(detail_line("Node class", node_class_name(node.class)));

            if let Some(attrs) = &node.attrs {
                if let Some(d) = &attrs.display_name {
                    lines.push(detail_line("Display name", d));
                }
                if let Some(dt) = &attrs.data_type {
                    let name = crate::filters::predicate::builtin_datatype_name(dt)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| dt.to_string());
                    lines.push(detail_line("Data type", &name));
                }
                if let Some(vr) = attrs.value_rank {
                    lines.push(detail_line("Value rank", &vr.to_string()));
                }
                lines.push(detail_line(
                    "Access",
                    &format!(
                        "{}{}{}",
                        if attrs.is_readable() { "read " } else { "" },
                        if attrs.is_writable() { "write " } else { "" },
                        if attrs.is_historizing() {
                            "historizing"
                        } else {
                            ""
                        },
                    ),
                ));
                lines.push(detail_line(
                    "Subscribable",
                    if is_subscribable(attrs) { "yes" } else { "no" },
                ));
                if let Some(desc) = &attrs.description
                    && !desc.is_empty()
                {
                    lines.push(detail_line("Description", desc));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "(loading attributes…)",
                    Style::default().fg(Color::DarkGray),
                )));
            }

            if let Some(value) = &node.value {
                lines.push(detail_line("Value", value));
            }
        } else {
            lines.push(Line::from("No node selected"));
        }

        let detail = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Details"))
            .wrap(Wrap { trim: true });
        f.render_widget(detail, area);
    }

    fn render_footer(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let text = if self.filter_mode {
            format!("/{}", self.filter_query)
        } else if self.acquire_mode {
            format!("acquire> {}", self.acquire_query)
        } else {
            self.status.clone()
        };
        let footer = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Color::Black).bg(Color::Gray),
        )));
        f.render_widget(footer, area);
    }
}

/// A legend entry: key, short action, priority (lower is more important).
type LegendEntry = (&'static str, &'static str, u8);

/// One key binding, shown in the `?` overlay and, when `legend` is set, in the
/// key legend row.
struct KeyHelp {
    group: &'static str,
    key: &'static str,
    description: &'static str,
    /// Legend key, short action and priority.
    legend: Option<LegendEntry>,
}

/// Every key of the explorer while browsing, in display order. The legend and
/// the help overlay are both rendered from this table.
const KEY_HELP: &[KeyHelp] = &[
    KeyHelp {
        group: "Navigation",
        key: "↑ ↓  k j",
        description: "move the selection",
        legend: Some(("↑↓", "move", 1)),
    },
    KeyHelp {
        group: "Navigation",
        key: "→  ⏎  l",
        description: "expand (browses on demand)",
        legend: Some(("→", "expand", 2)),
    },
    KeyHelp {
        group: "Navigation",
        key: "←  h",
        description: "collapse / go to parent",
        legend: Some(("←", "collapse", 3)),
    },
    KeyHelp {
        group: "Navigation",
        key: "g  G",
        description: "jump to top / bottom",
        legend: None,
    },
    KeyHelp {
        group: "Navigation",
        key: "/",
        description: "filter the tree by path",
        legend: Some(("/", "filter", 7)),
    },
    KeyHelp {
        group: "Navigation",
        key: "V",
        description: "only branches with variables",
        legend: Some(("V", "vars only", 11)),
    },
    KeyHelp {
        group: "Marking",
        key: "space",
        description: "mark / unmark with subtree",
        legend: Some(("space", "mark", 4)),
    },
    KeyHelp {
        group: "Marking",
        key: "S",
        description: "mark to subscribe (acquire)",
        legend: Some(("S/P", "sub/poll mark", 12)),
    },
    KeyHelp {
        group: "Marking",
        key: "P",
        description: "mark to poll (acquire)",
        legend: None,
    },
    KeyHelp {
        group: "Actions",
        key: "t",
        description: "tedge-dot library (--library)",
        legend: Some(("t", "library", 5)),
    },
    KeyHelp {
        group: "Actions",
        key: "a",
        description: "acquire config from marks",
        legend: Some(("a", "acquire", 6)),
    },
    KeyHelp {
        group: "Actions",
        key: "e",
        description: "export nodes (--file)",
        legend: Some(("e", "export", 8)),
    },
    KeyHelp {
        group: "Actions",
        key: "v",
        description: "read the selected value",
        legend: Some(("v", "value", 9)),
    },
    KeyHelp {
        group: "Actions",
        key: "s",
        description: "check it can be subscribed",
        legend: Some(("s", "validate", 13)),
    },
    KeyHelp {
        group: "Actions",
        key: "x",
        description: "cancel the running job",
        legend: None,
    },
    KeyHelp {
        group: "Other",
        key: "?",
        description: "show / hide this help",
        legend: None,
    },
    KeyHelp {
        group: "Other",
        key: "q  Esc  Ctrl+C",
        description: "quit",
        legend: Some(("q", "quit", 10)),
    },
];

/// What the mark column means.
const MARK_HELP: &[(&str, &str)] = &[
    ("[x]", "marked"),
    ("[S]", "subscribe"),
    ("[P]", "poll"),
    ("[+]", "inside a marked node"),
    ("[ ]", "not marked"),
];

/// The legend entries that fit `width`, most important first, shown in their
/// original order and followed by `? help` when `with_help`.
fn fit_legend(
    entries: &[LegendEntry],
    with_help: bool,
    width: usize,
) -> Vec<(&'static str, &'static str)> {
    let cost = |key: &str, label: &str| key.chars().count() + 1 + label.chars().count() + 2;
    let mut budget = width.saturating_sub(if with_help { cost("?", "help") } else { 0 });
    let mut by_priority: Vec<usize> = (0..entries.len()).collect();
    by_priority.sort_by_key(|&i| entries[i].2);
    let mut keep = vec![false; entries.len()];
    for i in by_priority {
        let c = cost(entries[i].0, entries[i].1);
        if c <= budget {
            budget -= c;
            keep[i] = true;
        }
    }
    let mut kept: Vec<(&'static str, &'static str)> = entries
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(e, _)| (e.0, e.1))
        .collect();
    if with_help {
        kept.push(("?", "help"));
    }
    kept
}

/// Width of the key column in the help overlay.
const HELP_KEY_WIDTH: usize = 15;
/// Width of one column of the help overlay (key + description).
const HELP_COLUMN_WIDTH: usize = 46;

/// One group of the help overlay as styled lines: a heading, then one line per key.
fn help_group(group: &str) -> Vec<Vec<Span<'static>>> {
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let head_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![vec![Span::styled(group.to_string(), head_style)]];
    for k in KEY_HELP.iter().filter(|k| k.group == group) {
        lines.push(vec![
            Span::styled(format!("  {:<w$}", k.key, w = HELP_KEY_WIDTH), key_style),
            Span::raw(k.description),
        ]);
    }
    lines
}

/// Pad a line's spans with spaces to `width` characters.
fn pad_spans(mut spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let len: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if len < width {
        spans.push(Span::raw(" ".repeat(width - len)));
    }
    spans
}

/// The help overlay text: every binding by group, then the mark symbols. With
/// `two_columns`, Navigation and Marking sit left of Actions and Other.
fn help_lines(two_columns: bool) -> Vec<Line<'static>> {
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let head_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::new();
    let column = |groups: &[&str]| -> Vec<Vec<Span<'static>>> {
        let mut out = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            if i > 0 {
                out.push(Vec::new());
            }
            out.extend(help_group(g));
        }
        out
    };
    if two_columns {
        let left = column(&["Navigation", "Marking"]);
        let right = column(&["Actions", "Other"]);
        for i in 0..left.len().max(right.len()) {
            let mut spans = pad_spans(left.get(i).cloned().unwrap_or_default(), HELP_COLUMN_WIDTH);
            spans.push(Span::raw("  "));
            spans.extend(right.get(i).cloned().unwrap_or_default());
            lines.push(Line::from(spans));
        }
    } else {
        lines.extend(
            column(&["Navigation", "Marking", "Actions", "Other"])
                .into_iter()
                .map(Line::from),
        );
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Marks", head_style));
    // As many marks per line as the box is wide.
    let inner = if two_columns {
        2 * HELP_COLUMN_WIDTH + 2
    } else {
        HELP_COLUMN_WIDTH
    };
    let mut marks: Vec<Span> = vec![Span::raw("  ")];
    let mut used = 2;
    for (mark, meaning) in MARK_HELP {
        let entry = format!("{mark} {meaning}").chars().count();
        if used > 2 && used + entry > inner {
            lines.push(Line::from(std::mem::take(&mut marks)));
            marks.push(Span::raw("  "));
            used = 2;
        }
        marks.push(Span::styled(*mark, key_style));
        marks.push(Span::raw(format!(" {meaning}   ")));
        used += entry + 3;
    }
    lines.push(Line::from(marks));
    lines
}

/// Draw the help overlay centered over the UI: two columns when they fit,
/// scrolled by `scroll` lines when it is taller than the screen. Returns the
/// scroll offset clamped to the content.
fn render_help(f: &mut Frame, scroll: u16) -> u16 {
    let area = f.area();
    let two_columns = area.width as usize >= 2 * HELP_COLUMN_WIDTH + 4;
    let lines = help_lines(two_columns);
    let wanted = if two_columns {
        2 * HELP_COLUMN_WIDTH + 4
    } else {
        HELP_COLUMN_WIDTH + 2
    };
    let width = (wanted as u16).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let hidden = (lines.len() as u16 + 2).saturating_sub(height);
    let scroll = scroll.min(hidden);
    let title = if hidden > 0 {
        " Keys — ↑↓ scroll · ? / Esc / q to close "
    } else {
        " Keys — ? / Esc / q to close "
    };
    let popup = ratatui::layout::Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(title)),
        popup,
    );
    scroll
}

/// Walk the tree depth-first collecting visible (expanded-chain) node indices.
fn collect_visible(arena: &[Node], idx: usize, out: &mut Vec<usize>) {
    out.push(idx);
    if arena[idx].expanded {
        for &child in &arena[idx].children {
            collect_visible(arena, child, out);
        }
    }
}

/// Browse one node's children, prune excluded paths, and batch-read attributes
/// in a single service call. Shared by the synchronous bootstrap load and the
/// background browse job so neither does per-child round-trips.
async fn browse_into_children(
    session: &Session,
    filters: &FilterSet,
    parent_id: &NodeId,
    parent_path: &str,
    page_size: u32,
) -> std::result::Result<Vec<LoadedChild>, String> {
    let refs = browse_children(session, parent_id, page_size)
        .await
        .map_err(|e| e.to_string())?;

    // Build paths and drop excluded branches before reading attributes.
    let mut kept: Vec<(ReferenceDescription, NodeId, String, String)> = Vec::new();
    for reference in refs {
        let node_id = reference.node_id.node_id.clone();
        let segment = segment_of(&reference, &node_id);
        let path = if parent_path.is_empty() {
            segment.clone()
        } else {
            format!("{parent_path}/{segment}")
        };
        if filters.path_decision(&path).pruned {
            continue;
        }
        kept.push((reference, node_id, segment, path));
    }

    // One batched attribute read for every surviving child.
    let child_ids: Vec<NodeId> = kept.iter().map(|(_, id, _, _)| id.clone()).collect();
    let attrs: Vec<Option<NodeAttributes>> = match read_node_attributes(session, &child_ids).await {
        Ok(a) => a.into_iter().map(Some).collect(),
        Err(_) => vec![None; child_ids.len()],
    };

    let children = kept
        .into_iter()
        .zip(attrs)
        .map(|((reference, node_id, segment, path), attr)| {
            let class = reference.node_class;
            LoadedChild {
                node_id,
                label: segment,
                class,
                path,
                expandable: is_expandable(class),
                attrs: attr,
            }
        })
        .collect();
    Ok(children)
}

/// A subtree root selected for export.
struct ExportRoot {
    node_id: NodeId,
    path: String,
    record: NodeRecord,
    /// Acquisition mode for the marked subtree (ignored by JSON/CSV export).
    mode: NodeMode,
}

/// Traversal settings reused by the background export's concurrent engine.
struct ExportTemplate {
    strategy: crate::cli::Strategy,
    workers: usize,
    page_size: u32,
    max_children: Option<usize>,
    retry: u32,
    preview_values: bool,
}

/// Background export: run a concurrent [`crate::scanner::Engine`] scan rooted at
/// each marked subtree, stream progress to the UI, and write the file. Reusing
/// the engine means the export benefits from the same worker-pool concurrency
/// as the non-interactive `browse` command.
#[allow(clippy::too_many_arguments)]
async fn export_job(
    session: Arc<Session>,
    filters: Arc<FilterSet>,
    namespaces: Arc<NamespaceTable>,
    roots: Vec<ExportRoot>,
    tmpl: ExportTemplate,
    path: PathBuf,
    encoding: Encoding,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<JobMsg>,
) {
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;

    let mut records: Vec<NodeRecord> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut processed = 0usize;

    for root in roots {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        // Include the marked node itself, then its full subtree.
        if seen.insert(root.record.node_id.clone()) {
            records.push(root.record.clone());
            processed += 1;
        }

        let config = Arc::new(crate::scanner::ScanConfig {
            root: root.node_id.clone(),
            // Always traverse the whole marked subtree regardless of the
            // interactive depth, since the user chose this branch explicitly.
            depth: crate::cli::Depth::Unlimited,
            strategy: tmpl.strategy,
            workers: tmpl.workers,
            page_size: tmpl.page_size,
            max_children: tmpl.max_children,
            retry: tmpl.retry,
            preview_values: tmpl.preview_values,
        });
        let engine = crate::scanner::Engine::new(
            session.clone(),
            config,
            filters.clone(),
            namespaces.clone(),
            cancel.clone(),
        );

        let (rtx, mut rrx) = mpsc::channel::<NodeRecord>(1024);
        let handle = tokio::spawn(async move { engine.run(rtx).await });

        while let Some(mut record) = rrx.recv().await {
            // Engine paths are relative to its root; restore the absolute path.
            if !root.path.is_empty() {
                record.browse_path = format!("{}/{}", root.path, record.browse_path);
            }
            if seen.insert(record.node_id.clone()) {
                records.push(record);
                processed += 1;
                if processed.is_multiple_of(64) {
                    let _ = tx.send(JobMsg::Progress { processed }).await;
                }
            }
        }
        let _ = handle.await;
    }

    let result = write_export(&mut records, encoding, Some(&path))
        .map(|()| records.len())
        .map_err(|e| e.to_string());
    let _ = tx.send(JobMsg::ExportDone { result, path }).await;
}

/// Background collection: run a concurrent scan over each marked subtree and
/// gather the node ids of all Variable nodes (de-duplicated) for an acquire
/// config. Mirrors [`export_job`] but yields node ids instead of writing.
async fn collect_job(
    session: Arc<Session>,
    filters: Arc<FilterSet>,
    namespaces: Arc<NamespaceTable>,
    roots: Vec<ExportRoot>,
    tmpl: ExportTemplate,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<JobMsg>,
) {
    use std::collections::HashSet;
    use std::sync::atomic::Ordering;

    let mut nodes: Vec<(NodeRecord, Option<GenMode>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut processed = 0usize;

    for root in roots {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        let root_mode = gen_mode_of(root.mode);
        // Include the marked node itself if it is a Variable or Method.
        if is_point_record(&root.record) && seen.insert(root.record.node_id.clone()) {
            nodes.push((root.record.clone(), root_mode));
        }

        let config = Arc::new(crate::scanner::ScanConfig {
            root: root.node_id.clone(),
            depth: crate::cli::Depth::Unlimited,
            strategy: tmpl.strategy,
            workers: tmpl.workers,
            page_size: tmpl.page_size,
            max_children: tmpl.max_children,
            retry: tmpl.retry,
            preview_values: false,
        });
        let engine = crate::scanner::Engine::new(
            session.clone(),
            config,
            filters.clone(),
            namespaces.clone(),
            cancel.clone(),
        );

        let (rtx, mut rrx) = mpsc::channel::<NodeRecord>(1024);
        let handle = tokio::spawn(async move { engine.run(rtx).await });

        while let Some(mut record) = rrx.recv().await {
            processed += 1;
            // Engine paths are relative to its root; restore the absolute path.
            if !root.path.is_empty() {
                record.browse_path = format!("{}/{}", root.path, record.browse_path);
            }
            if is_point_record(&record) && seen.insert(record.node_id.clone()) {
                nodes.push((record, root_mode));
            }
            if processed.is_multiple_of(64) {
                let _ = tx.send(JobMsg::Progress { processed }).await;
            }
        }
        let _ = handle.await;
    }

    let _ = tx.send(JobMsg::CollectDone { result: Ok(nodes) }).await;
}

/// Node classes that can become tedge-dot points.
fn is_point_class(class: NodeClass) -> bool {
    matches!(class, NodeClass::Variable | NodeClass::Method)
}

fn is_point_record(record: &NodeRecord) -> bool {
    matches!(record.node_class.as_str(), "Variable" | "Method")
}

/// Build a GenNode from a discovered record, capturing the namespace URI.
fn record_to_gennode(
    record: &NodeRecord,
    mode: Option<GenMode>,
) -> crate::acquire::generate::GenNode {
    crate::acquire::generate::GenNode {
        id: record.node_id.clone(),
        namespace: Some(record.namespace_uri.clone()).filter(|s| !s.is_empty()),
        name: Some(record.display_name.clone()).filter(|s| !s.is_empty()),
        path: Some(record.browse_path.clone()).filter(|s| !s.is_empty()),
        mode,
    }
}

/// Map a per-node [`NodeMode`] to a [`GenMode`] for config generation; `Auto`
/// has no explicit mode and follows the config default.
fn gen_mode_of(mode: NodeMode) -> Option<GenMode> {
    match mode {
        NodeMode::Auto => None,
        NodeMode::Subscribe => Some(GenMode::Subscribe),
        NodeMode::Poll => Some(GenMode::Poll),
    }
}

/// Parse an acquire prompt of the form `<poll|subscribe> <interval>` (interval
/// defaults to 1s when omitted).
fn parse_acquire_query(
    query: &str,
) -> std::result::Result<(crate::acquire::generate::GenMode, Duration), String> {
    use crate::acquire::generate::GenMode;

    let mut parts = query.split_whitespace();
    let mode = match parts.next() {
        Some(m) if m.eq_ignore_ascii_case("poll") => GenMode::Poll,
        Some(m) if m.eq_ignore_ascii_case("subscribe") || m.eq_ignore_ascii_case("sub") => {
            GenMode::Subscribe
        }
        Some(other) => return Err(format!("unknown mode '{other}' (use poll or subscribe)")),
        None => return Err("expected '<poll|subscribe> <interval>'".to_string()),
    };
    let interval = match parts.next() {
        Some(s) => humantime::parse_duration(s).map_err(|e| format!("invalid interval: {e}"))?,
        None => Duration::from_secs(1),
    };
    Ok((mode, interval))
}

/// A node class likely to have hierarchical children worth expanding.
fn is_expandable(class: NodeClass) -> bool {
    matches!(
        class,
        NodeClass::Object
            | NodeClass::Variable
            | NodeClass::View
            | NodeClass::ObjectType
            | NodeClass::VariableType
    )
}

/// Whether this tree node is a Variable the server reports as *not* currently
/// readable: such nodes only ever return `BadNotReadable`, so they must not be
/// written into an acquire config. Unknown access levels are treated as
/// readable, so nodes are only excluded on explicit evidence.
fn node_is_unreadable_variable(node: &Node) -> bool {
    node.class == NodeClass::Variable
        && node
            .attrs
            .as_ref()
            .map(|a| a.is_unreadable_variable())
            .unwrap_or(false)
}

/// Same as [`node_is_unreadable_variable`] but for a discovered [`NodeRecord`]
/// (used by the background subtree collection, which works on records).
fn record_is_unreadable_variable(record: &NodeRecord) -> bool {
    record.node_class == "Variable"
        && record
            .access_level
            .map(|a| a & crate::opcua::attributes::access::CURRENT_READ == 0)
            .unwrap_or(false)
}

/// Short class tag for the tree list.
fn short_class(class: NodeClass) -> &'static str {
    match class {
        NodeClass::Object => "[obj]",
        NodeClass::Variable => "[var]",
        NodeClass::Method => "[mth]",
        NodeClass::ObjectType => "[objT]",
        NodeClass::VariableType => "[varT]",
        NodeClass::ReferenceType => "[refT]",
        NodeClass::DataType => "[dt]",
        NodeClass::View => "[view]",
        NodeClass::Unspecified => "",
    }
}

/// Browse-path segment for a reference (browse name preferred).
fn segment_of(reference: &ReferenceDescription, node_id: &NodeId) -> String {
    let browse = reference.browse_name.name.to_string();
    if !browse.is_empty() {
        return browse;
    }
    let display = reference.display_name.text.to_string();
    if !display.is_empty() {
        return display;
    }
    node_id.to_string()
}

/// A `label: value` detail line.
fn detail_line<'a>(label: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:>13}: "), Style::default().fg(Color::Cyan)),
        Span::raw(value.to_string()),
    ])
}

/// Pick an export encoding from a file extension.
fn encoding_for(path: &std::path::Path) -> crate::cli::Encoding {
    match path.extension().and_then(|e| e.to_str()) {
        Some("csv") => crate::cli::Encoding::Csv,
        Some("ndjson") => crate::cli::Encoding::Ndjson,
        _ => crate::cli::Encoding::Json,
    }
}

#[cfg(test)]
mod legend_tests {
    use super::*;

    fn browsing() -> Vec<LegendEntry> {
        KEY_HELP.iter().filter_map(|k| k.legend).collect()
    }

    #[test]
    fn wide_terminal_shows_every_legend_entry() {
        let kept = fit_legend(&browsing(), true, 400);
        assert_eq!(kept.len(), browsing().len() + 1);
        assert_eq!(kept.last(), Some(&("?", "help")));
    }

    #[test]
    fn narrow_terminal_keeps_help_and_the_most_important_keys() {
        let kept = fit_legend(&browsing(), true, 60);
        assert_eq!(kept.last(), Some(&("?", "help")));
        assert!(kept.contains(&("↑↓", "move")) && kept.contains(&("space", "mark")));
        assert!(!kept.contains(&("s", "validate")));
        let width: usize = kept
            .iter()
            .map(|(k, l)| k.chars().count() + 1 + l.chars().count() + 2)
            .sum();
        assert!(width <= 60 + 2, "{kept:?}");
    }

    #[test]
    fn tiny_terminal_still_shows_help() {
        assert_eq!(fit_legend(&browsing(), true, 5), vec![("?", "help")]);
    }

    #[test]
    fn overlay_lists_every_binding_and_mark() {
        for two_columns in [false, true] {
            let text: String = help_lines(two_columns)
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                        + "\n"
                })
                .collect();
            for k in KEY_HELP {
                assert!(
                    text.contains(k.key) && text.contains(k.description),
                    "{}",
                    k.key
                );
            }
            for (mark, _) in MARK_HELP {
                assert!(text.contains(mark), "{mark}");
            }
        }
    }

    #[test]
    fn overlay_lines_fit_their_box() {
        let width = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
        };
        for (two, inner) in [
            (true, 2 * HELP_COLUMN_WIDTH + 2),
            (false, HELP_COLUMN_WIDTH),
        ] {
            for l in help_lines(two).iter() {
                // Trailing padding is invisible.
                let pad = l
                    .spans
                    .last()
                    .map_or(0, |s| s.content.len() - s.content.trim_end().len());
                assert!(
                    width(l) - pad <= inner,
                    "{} > {inner}: {l:?}",
                    width(l) - pad
                );
            }
        }
    }

    #[test]
    fn two_column_overlay_fits_a_24_line_terminal() {
        // Borders take two rows; the legend and status rows stay visible below.
        assert!(
            help_lines(true).len() + 2 <= 22,
            "{}",
            help_lines(true).len()
        );
    }
}
