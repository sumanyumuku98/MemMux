//! The Elm-architecture core of the TUI (SUM-81): a `Model`, an abstract `Key`, and a pure
//! `update` that returns the next model plus [`Effect`]s for the runtime to execute. Keeping
//! `update` free of terminal and socket I/O makes the whole interaction layer unit-testable.
//!
//! The UI is a single **Home** view (SUM-127/134): a two-section sidebar — a WORKSPACES panel on
//! top and an AGENTS panel (agents grouped under workspace headers) below — plus modals for
//! creating a task / opening a folder / help. There are no numbered tabs. `Tab` toggles the
//! focused section; Enter on an agent opens it as a pane, and open panes are scoped per workspace
//! (only the active group is shown).

use memmux_proto::{
    CreateTaskRequest, DaemonInfo, EventView, PressureView, TaskView, WorkspaceView,
};

/// Provider slugs offered by the new-task form's picker (SUM-87).
pub const PROVIDERS: [&str; 5] = ["claude-code", "codex", "gemini-cli", "opencode", "generic"];

/// The quick-launch palette entries (SUM-130): a friendly label, the provider slug to create, and
/// whether the entry is a plain interactive shell (the runtime fills in `$SHELL` at launch, since
/// the generic adapter with no command exits immediately). Not restricted to one agent.
pub const LAUNCH_ITEMS: [(&str, &str, bool); 5] = [
    ("claude-code", "claude-code", false),
    ("codex", "codex", false),
    ("gemini-cli", "gemini-cli", false),
    ("opencode", "opencode", false),
    ("shell", "generic", true),
];

/// A destructive action awaiting confirmation in [`View::Confirm`] (SUM-130).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingAction {
    /// Terminate a running/queued agent (kills the provider, preserves a dirty worktree).
    Terminate(String),
    /// Forget a terminal agent (removes it from the list/registry entirely).
    Forget(String),
}

// ---- Pane multiplexer (SUM-132) ---------------------------------------------------------------

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// A spatial direction for pane focus movement (the leader + h/j/k/l).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Left.
    Left,
    /// Right.
    Right,
    /// Up.
    Up,
    /// Down.
    Down,
}

/// How a split divides its area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orient {
    /// Side by side (a | b) — a "split right".
    Cols,
    /// Stacked (a over b) — a "split down".
    Rows,
}

/// Whether keyboard focus is on the sidebar or inside the pane grid (SUM-132).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The workspace/agent sidebar.
    Sidebar,
    /// The pane grid (keys go to the focused agent).
    Panes,
}

/// The pane-command leader key, configurable via `~/.memmux/config.toml` (SUM-133). Default
/// `Ctrl-b` (tmux's leader) — `Ctrl-a` is avoided because shells/readline swallow it as
/// "beginning of line". Held as plain data so `app` stays crossterm-free; the runtime matches
/// real key events against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefix {
    /// Whether Ctrl must be held.
    pub ctrl: bool,
    /// The (lowercase ASCII) key.
    pub ch: char,
}

impl Default for Prefix {
    fn default() -> Self {
        Self {
            ctrl: true,
            ch: 'b',
        }
    }
}

impl Prefix {
    /// Parse a spec like `"ctrl-b"`, `"ctrl+b"`, `"c-b"`, or a bare `"b"`. Returns `None` for
    /// anything but a single ASCII letter (callers keep the default).
    pub fn parse(s: &str) -> Option<Prefix> {
        let s = s.trim().to_ascii_lowercase();
        let (ctrl, rest) = if let Some(r) = s
            .strip_prefix("ctrl-")
            .or_else(|| s.strip_prefix("ctrl+"))
            .or_else(|| s.strip_prefix("c-"))
        {
            (true, r.to_string())
        } else {
            (false, s)
        };
        let mut chars = rest.chars();
        let ch = chars.next()?;
        if chars.next().is_some() || !ch.is_ascii_alphabetic() {
            return None;
        }
        Some(Prefix { ctrl, ch })
    }

    /// Human label for help/hints, e.g. `"Ctrl-b"`.
    pub fn label(&self) -> String {
        if self.ctrl {
            format!("Ctrl-{}", self.ch)
        } else {
            self.ch.to_ascii_uppercase().to_string()
        }
    }

    /// The byte to forward when the prefix is pressed twice (a literal prefix to the agent).
    pub fn literal_byte(&self) -> u8 {
        if self.ctrl {
            (self.ch as u8) & 0x1f
        } else {
            self.ch as u8
        }
    }
}

/// One cell of a rendered pane grid (a client-side snapshot of a vt100 screen — SUM-132).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridCell {
    /// The glyph.
    pub ch: char,
    /// Foreground colour.
    pub fg: ratatui::style::Color,
    /// Background colour.
    pub bg: ratatui::style::Color,
    /// Style modifiers (bold/italic/underline/reverse).
    pub mods: ratatui::style::Modifier,
}

/// A snapshot of an agent's terminal screen, drawn into a pane (SUM-132). Runtime-produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyledGrid {
    /// Rows of styled cells.
    pub rows: Vec<Vec<GridCell>>,
    /// Cursor `(row, col)`.
    pub cursor: (u16, u16),
    /// Whether the agent process is still alive.
    pub alive: bool,
}

/// A binary tiling tree of open agent panes (SUM-132). `Leaf` holds a task id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneLayout {
    /// A single agent pane (task id).
    Leaf(String),
    /// A split of two sub-layouts.
    Split {
        /// Split orientation.
        orient: Orient,
        /// Percent of the split given to child `a` (left/top); `b` gets the rest. Clamped to `10..=90`.
        ratio: u16,
        /// First child (left/top).
        a: Box<PaneLayout>,
        /// Second child (right/bottom).
        b: Box<PaneLayout>,
    },
}

impl PaneLayout {
    /// All leaf task ids, left-to-right / top-to-bottom.
    pub fn leaves(&self) -> Vec<String> {
        match self {
            PaneLayout::Leaf(id) => vec![id.clone()],
            PaneLayout::Split { a, b, .. } => {
                let mut v = a.leaves();
                v.extend(b.leaves());
                v
            }
        }
    }

    /// Whether `id` is present.
    pub fn contains(&self, id: &str) -> bool {
        self.leaves().iter().any(|l| l == id)
    }

    /// Split the leaf `target` into `[target | new_id]` (or stacked) with `new_id` focused.
    fn split_leaf(&mut self, target: &str, new_id: &str, orient: Orient) -> bool {
        match self {
            PaneLayout::Leaf(id) if id == target => {
                let a = Box::new(PaneLayout::Leaf(id.clone()));
                let b = Box::new(PaneLayout::Leaf(new_id.to_string()));
                *self = PaneLayout::Split {
                    orient,
                    ratio: 50,
                    a,
                    b,
                };
                true
            }
            PaneLayout::Leaf(_) => false,
            PaneLayout::Split { a, b, .. } => {
                a.split_leaf(target, new_id, orient) || b.split_leaf(target, new_id, orient)
            }
        }
    }

    /// Remove leaf `id`, collapsing its parent split into the sibling. Returns the new tree
    /// (`None` if the whole tree was just that leaf).
    fn without(self, id: &str) -> Option<PaneLayout> {
        match self {
            PaneLayout::Leaf(l) if l == id => None,
            PaneLayout::Leaf(l) => Some(PaneLayout::Leaf(l)),
            PaneLayout::Split {
                orient,
                ratio,
                a,
                b,
            } => {
                let a2 = a.without(id);
                let b2 = b.without(id);
                match (a2, b2) {
                    (Some(a), Some(b)) => Some(PaneLayout::Split {
                        orient,
                        ratio,
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    /// Compute each leaf's rectangle within `area` (shared by render + spatial focus + resize).
    pub fn leaf_rects(&self, area: Rect) -> Vec<(String, Rect)> {
        let mut out = Vec::new();
        self.collect_rects(area, &mut out);
        out
    }

    fn collect_rects(&self, area: Rect, out: &mut Vec<(String, Rect)>) {
        match self {
            PaneLayout::Leaf(id) => out.push((id.clone(), area)),
            PaneLayout::Split {
                orient,
                ratio,
                a,
                b,
            } => {
                let dir = match orient {
                    Orient::Cols => Direction::Horizontal,
                    Orient::Rows => Direction::Vertical,
                };
                let r = (*ratio).clamp(10, 90);
                let parts = Layout::default()
                    .direction(dir)
                    .constraints([Constraint::Percentage(r), Constraint::Percentage(100 - r)])
                    .split(area);
                a.collect_rects(parts[0], out);
                b.collect_rects(parts[1], out);
            }
        }
    }

    /// Move the boundary of the split governing `focused` along `axis_cols` by `delta` percent
    /// (SUM-135). Adjusts the *nearest* ancestor split whose orientation matches the axis
    /// (`Cols`↔horizontal). Returns whether a split was adjusted; ratio is clamped to `10..=90`.
    fn resize(&mut self, focused: &str, axis_cols: bool, delta: i16) -> bool {
        let PaneLayout::Split {
            orient,
            ratio,
            a,
            b,
        } = self
        else {
            return false;
        };
        let in_a = a.contains(focused);
        if !in_a && !b.contains(focused) {
            return false;
        }
        // Prefer the split closest to the focused leaf.
        let child = if in_a { a } else { b };
        if child.resize(focused, axis_cols, delta) {
            return true;
        }
        if (*orient == Orient::Cols) == axis_cols {
            *ratio = (*ratio as i16 + delta).clamp(10, 90) as u16;
            return true;
        }
        false
    }
}

/// The top-level views. One home layout + modals (SUM-127).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Sidebar of workspaces → agents + a main detail pane.
    Home,
    /// New-task form with provider picker (SUM-87).
    NewTask,
    /// One-key quick-launch palette: pick a provider (or a shell) and drop straight in (SUM-130).
    Launch,
    /// Confirm a destructive action (terminate / forget) before it runs (SUM-130).
    Confirm,
    /// Interactive folder browser to register a workspace (SUM-130, replaces the typed path).
    OpenFolder,
    /// Help / keymap.
    Help,
}

/// An abstract key, decoupled from crossterm so `update` is testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Move selection up.
    Up,
    /// Move selection down.
    Down,
    /// Previous option / scroll left.
    Left,
    /// Next option / scroll right.
    Right,
    /// Confirm.
    Enter,
    /// Cancel / back.
    Esc,
    /// Cycle form field.
    Tab,
    /// Delete a character in a form field.
    Backspace,
    /// A typed character.
    Char(char),
    /// Quit the app.
    Quit,
}

/// Which field the new-task form is editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormField {
    /// Task title.
    Title,
    /// Repository path.
    Repo,
    /// Provider (picker).
    Provider,
    /// Base branch.
    Base,
}

/// New-task form state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTaskForm {
    /// Title text.
    pub title: String,
    /// Repository path text.
    pub repo: String,
    /// Selected provider index into [`PROVIDERS`].
    pub provider_idx: usize,
    /// Base branch text.
    pub base: String,
    /// Focused field.
    pub field: FormField,
}

impl Default for NewTaskForm {
    fn default() -> Self {
        Self {
            title: String::new(),
            repo: String::new(),
            provider_idx: 0,
            base: "main".to_string(),
            field: FormField::Title,
        }
    }
}

impl NewTaskForm {
    /// The selected provider slug.
    pub fn provider(&self) -> &'static str {
        PROVIDERS[self.provider_idx.min(PROVIDERS.len() - 1)]
    }

    /// Build a create request if the form is complete enough. Title is optional (SUM-119): a blank
    /// title is sent empty and the daemon auto-derives one; only the repo is required.
    pub fn to_request(&self) -> Option<CreateTaskRequest> {
        if self.repo.trim().is_empty() {
            return None;
        }
        Some(CreateTaskRequest {
            title: self.title.trim().to_string(),
            repository_path: self.repo.trim().to_string(),
            provider: self.provider().to_string(),
            base_branch: if self.base.trim().is_empty() {
                "main".into()
            } else {
                self.base.trim().to_string()
            },
            resource_class: None,
            priority: None,
            command: None,
        })
    }
}

/// Live data fetched from the daemon.
#[derive(Clone, Debug, Default)]
pub struct Data {
    /// Tasks (agents).
    pub tasks: Vec<TaskView>,
    /// Pressure snapshot.
    pub pressure: Option<PressureView>,
    /// Daemon info.
    pub daemon: Option<DaemonInfo>,
    /// Recent events (newest last).
    pub events: Vec<EventView>,
    /// Registered workspaces (SUM-124).
    pub workspaces: Vec<WorkspaceView>,
}

/// A row in the sidebar's grouped AGENTS panel: workspace headers followed by their agents (SUM-134).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavItem {
    /// A registered workspace header (index into [`Data::workspaces`]).
    Workspace(usize),
    /// Header for agents with no registered workspace.
    Unregistered,
    /// An agent (index into [`Data::tasks`]).
    Agent(usize),
}

/// A row in the sidebar's top WORKSPACES panel (SUM-134): a registered workspace or the catch-all
/// "Other" bucket for agents with no registered workspace (only present when such agents exist).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsRow {
    /// A registered workspace (index into [`Data::workspaces`]).
    Workspace(usize),
    /// The catch-all group for ungrouped agents.
    Other,
}

/// Which stacked sidebar section currently has keyboard focus (SUM-134). `Tab`/`BackTab` toggles
/// it; `j/k`/arrows move within the focused section only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarSection {
    /// The top WORKSPACES panel (drives the active pane group).
    Workspaces,
    /// The bottom AGENTS panel (agents grouped under workspace headers).
    Agents,
}

/// Sentinel workspace key for agents with no registered workspace (SUM-134). Uses a NUL prefix so
/// it can never collide with a real repository id and sorts first in the [`Model::panes`] map.
pub const OTHER_WS_KEY: &str = "\u{0}other";

/// A side effect for the runtime to perform (I/O the pure `update` cannot do).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Re-fetch data from the daemon.
    Refresh,
    /// Create a task.
    CreateTask(CreateTaskRequest),
    /// One-key quick-launch (SUM-130): create a task for `provider` in `repo`, start it, and attach
    /// — all from the runtime. `shell` requests a plain interactive shell (generic provider + `$SHELL`).
    QuickLaunch {
        /// Provider slug to create.
        provider: String,
        /// Target repository / directory.
        repo: String,
        /// Whether this is a plain shell rather than an agent.
        shell: bool,
    },
    /// Open an agent as a pane (SUM-132): the runtime starts-or-restarts the provider, then opens
    /// a live Attach session and adds it to the pane layout (focus moves to the pane grid). The
    /// pure layout mutation already happened in `update`/`open_pane`; this tells the runtime to
    /// bring the provider up and wire the session.
    OpenPane(String),
    /// Terminate a running/queued agent (SUM-130).
    TerminateTask(String),
    /// Forget a terminal agent, removing it from the registry (SUM-130).
    ForgetTask(String),
    /// List a directory for the workspace folder browser (SUM-130).
    ListDir(String),
    /// Register a folder as a workspace (SUM-124).
    AddWorkspace(String),
}

/// The full application model.
#[derive(Clone, Debug)]
pub struct Model {
    /// Current view.
    pub view: View,
    /// Loaded data.
    pub data: Data,
    /// Which stacked sidebar section has keyboard focus (SUM-134).
    pub sidebar: SidebarSection,
    /// Selected row index into the WORKSPACES panel ([`Model::ws_items`]) (SUM-134).
    pub ws_selected: usize,
    /// Selected row index into the AGENTS panel ([`Model::nav_items`]); only ever lands on an
    /// [`NavItem::Agent`] row (SUM-134).
    pub agent_selected: usize,
    /// New-task form.
    pub form: NewTaskForm,
    /// The git root of the directory `memmux` was launched from, if any — the default repo for the
    /// new-task form (SUM-119). Set by the runtime; `update` only reads it.
    pub cwd_repo: Option<String>,
    /// The directory `memmux` was launched from (even when not a git root) — the fallback target
    /// for a plain shell and the seed for the folder browser (SUM-130). Set by the runtime.
    pub cwd: Option<String>,
    /// Highlighted entry in the quick-launch palette (SUM-130).
    pub launch_selected: usize,
    /// Target repo/dir the quick-launch palette will launch into (SUM-130).
    pub launch_repo: String,
    /// Directory currently shown in the folder browser (SUM-130).
    pub browse_dir: String,
    /// Sub-directory entries in `browse_dir` (a leading `..` unless at the root) (SUM-130).
    pub browse_entries: Vec<String>,
    /// Highlighted entry in the folder browser (SUM-130).
    pub browse_selected: usize,
    /// A destructive action awaiting confirmation (SUM-130).
    pub pending: Option<PendingAction>,
    /// Whether keyboard focus is on the sidebar or the pane grid (SUM-132).
    pub focus: Focus,
    /// The configurable pane leader key (SUM-133); set by the runtime from config.
    pub prefix: Prefix,
    /// Leader pressed inside the pane grid, awaiting a command key (SUM-132).
    pub prefix_active: bool,
    /// Open agent panes, one tiling tree per workspace group (SUM-134), keyed by workspace key
    /// ([`Model::ws_key`]). Only the [`Model::active_ws`] group is shown; hidden groups keep their
    /// agents running. A group's key is dropped when its last pane closes.
    pub panes: std::collections::BTreeMap<String, PaneLayout>,
    /// The workspace key of the group currently shown in the grid (SUM-134).
    pub active_ws: Option<String>,
    /// The focused pane's task id, within the active group (SUM-132).
    pub focused_pane: Option<String>,
    /// Orientation for the next opened pane (toggled by leader v / leader -) (SUM-132).
    pub split_orient: Orient,
    /// Whether the focused pane is zoomed to fill the grid area (SUM-132).
    pub zoomed: bool,
    /// Whether resize mode is active (leader `r`): h/j/k/l adjust the focused split (SUM-135).
    pub resizing: bool,
    /// Live styled screens for open panes, snapshotted by the runtime each frame (SUM-132).
    pub pane_screens: std::collections::HashMap<String, StyledGrid>,
    /// Status line message.
    pub status: String,
    /// Whether to exit.
    pub should_quit: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            view: View::Home,
            data: Data::default(),
            sidebar: SidebarSection::Workspaces,
            ws_selected: 0,
            agent_selected: 0,
            form: NewTaskForm::default(),
            cwd_repo: None,
            cwd: None,
            launch_selected: 0,
            launch_repo: String::new(),
            browse_dir: String::new(),
            browse_entries: Vec::new(),
            browse_selected: 0,
            pending: None,
            focus: Focus::Sidebar,
            prefix: Prefix::default(),
            prefix_active: false,
            panes: std::collections::BTreeMap::new(),
            active_ws: None,
            focused_pane: None,
            split_orient: Orient::Cols,
            zoomed: false,
            resizing: false,
            pane_screens: std::collections::HashMap::new(),
            status: "connected".to_string(),
            should_quit: false,
        }
    }
}

impl Model {
    /// Whether any agent has no registered workspace (i.e. the "Other" group applies) (SUM-134).
    fn has_ungrouped(&self) -> bool {
        self.data
            .tasks
            .iter()
            .any(|t| !self.data.workspaces.iter().any(|w| w.id == t.repository))
    }

    /// The rows of the top WORKSPACES panel (SUM-134): every registered workspace, then an `Other`
    /// row when any agent is ungrouped. Drives the active pane group.
    pub fn ws_items(&self) -> Vec<WsRow> {
        let mut items: Vec<WsRow> = (0..self.data.workspaces.len())
            .map(WsRow::Workspace)
            .collect();
        if self.has_ungrouped() {
            items.push(WsRow::Other);
        }
        items
    }

    /// The currently selected WORKSPACES row, if any (SUM-134).
    pub fn selected_ws(&self) -> Option<WsRow> {
        self.ws_items().get(self.ws_selected).copied()
    }

    /// The flattened AGENTS list: each registered workspace header followed by its agents, then
    /// any agents with no registered workspace under an "unregistered" header.
    pub fn nav_items(&self) -> Vec<NavItem> {
        let mut items = Vec::new();
        let mut grouped = vec![false; self.data.tasks.len()];
        for (wi, ws) in self.data.workspaces.iter().enumerate() {
            items.push(NavItem::Workspace(wi));
            for (ti, t) in self.data.tasks.iter().enumerate() {
                if t.repository == ws.id {
                    items.push(NavItem::Agent(ti));
                    grouped[ti] = true;
                }
            }
        }
        let ungrouped: Vec<usize> = (0..self.data.tasks.len())
            .filter(|&i| !grouped[i])
            .collect();
        if !ungrouped.is_empty() {
            items.push(NavItem::Unregistered);
            for ti in ungrouped {
                items.push(NavItem::Agent(ti));
            }
        }
        items
    }

    /// The currently selected AGENTS row (SUM-134). Selection only ever lands on an agent, so this
    /// is that [`NavItem::Agent`]; `None` when there are no agents.
    pub fn selected_nav(&self) -> Option<NavItem> {
        self.nav_items().get(self.agent_selected).copied()
    }

    /// Whether a nav row is an agent (the only selectable AGENTS rows — headers are dim labels).
    fn nav_is_agent(item: NavItem) -> bool {
        matches!(item, NavItem::Agent(_))
    }

    /// Nudge the AGENTS selection off a header onto the nearest agent, searching in `dir` (+1 down,
    /// -1 up) then the other way. No-op when there are no agents (SUM-134).
    fn snap_agent_to_selectable(&mut self, dir: isize) {
        let nav = self.nav_items();
        if nav.is_empty() {
            self.agent_selected = 0;
            return;
        }
        let idx = self.agent_selected.min(nav.len() - 1);
        if nav.get(idx).copied().map(Self::nav_is_agent) == Some(true) {
            self.agent_selected = idx;
            return;
        }
        // Search forward in the requested direction, then fall back the other way.
        for &step in &[dir, -dir] {
            let mut i = idx as isize;
            loop {
                i += step;
                if i < 0 || i >= nav.len() as isize {
                    break;
                }
                if Self::nav_is_agent(nav[i as usize]) {
                    self.agent_selected = i as usize;
                    return;
                }
            }
        }
        self.agent_selected = idx;
    }

    /// Toggle the focused sidebar section (SUM-134).
    pub fn toggle_section(&mut self) {
        self.sidebar = match self.sidebar {
            SidebarSection::Workspaces => SidebarSection::Agents,
            SidebarSection::Agents => SidebarSection::Workspaces,
        };
    }

    /// Move the selection within the focused section by `delta` rows (SUM-134). In AGENTS the
    /// selection skips header rows, landing only on agents. A WORKSPACES move re-points the active
    /// pane group at the newly selected row.
    pub fn move_selection(&mut self, delta: isize) {
        match self.sidebar {
            SidebarSection::Workspaces => {
                let len = self.ws_items().len();
                if len == 0 {
                    return;
                }
                let next = (self.ws_selected as isize + delta).clamp(0, len as isize - 1);
                self.ws_selected = next as usize;
                self.sync_active_ws();
            }
            SidebarSection::Agents => {
                let nav = self.nav_items();
                if nav.is_empty() {
                    return;
                }
                // Step over header rows so the selection always lands on an agent.
                let step = delta.signum();
                if step == 0 {
                    return;
                }
                let mut i = self.agent_selected as isize;
                loop {
                    let next = i + step;
                    if next < 0 || next >= nav.len() as isize {
                        break;
                    }
                    i = next;
                    if Self::nav_is_agent(nav[i as usize]) {
                        self.agent_selected = i as usize;
                        break;
                    }
                }
            }
        }
    }

    /// The workspace key for the WORKSPACES row at `ws_selected`, if any (SUM-134).
    fn selected_ws_key(&self) -> Option<String> {
        match self.selected_ws()? {
            WsRow::Workspace(wi) => self.data.workspaces.get(wi).map(|w| w.id.clone()),
            WsRow::Other => Some(OTHER_WS_KEY.to_string()),
        }
    }

    /// Point [`Model::active_ws`] (and the focused pane) at the WORKSPACES selection's group
    /// (SUM-134). Focuses that group's first leaf, or `None` when it has no open panes.
    pub fn sync_active_ws(&mut self) {
        let Some(key) = self.selected_ws_key() else {
            return;
        };
        self.active_ws = Some(key.clone());
        self.focused_pane = self
            .panes
            .get(&key)
            .and_then(|l| l.leaves().first().cloned());
        self.zoomed = false;
    }

    /// The registered workspace path for the WORKSPACES selection, if it names a real workspace.
    fn selected_ws_path(&self) -> Option<String> {
        match self.selected_ws() {
            Some(WsRow::Workspace(wi)) => self.data.workspaces.get(wi).map(|w| w.path.clone()),
            _ => None,
        }
    }

    /// Default repo path for a new task (SUM-119/134): driven by the focused section — the selected
    /// workspace's path (WORKSPACES), or the selected agent's workspace (AGENTS) — else the launch
    /// directory's git root.
    pub fn default_repo(&self) -> String {
        match self.sidebar {
            SidebarSection::Workspaces => self
                .selected_ws_path()
                .or_else(|| self.cwd_repo.clone())
                .unwrap_or_default(),
            SidebarSection::Agents => match self.selected_nav() {
                Some(NavItem::Agent(ti)) => self
                    .data
                    .tasks
                    .get(ti)
                    .and_then(|t| self.data.workspaces.iter().find(|w| w.id == t.repository))
                    .map(|w| w.path.clone())
                    .or_else(|| self.cwd_repo.clone())
                    .unwrap_or_default(),
                _ => self.cwd_repo.clone().unwrap_or_default(),
            },
        }
    }

    /// The quick-launch target repo (SUM-130): the selection's repo, falling back to the launch
    /// git root and then the raw launch dir so a plain shell can start anywhere.
    pub fn launch_target_repo(&self) -> String {
        let repo = self.default_repo();
        if repo.is_empty() {
            self.cwd.clone().unwrap_or_default()
        } else {
            repo
        }
    }

    /// The registered-workspace path of the focused pane's agent, if any (SUM-136). Used so a
    /// pane split (leader v / leader -) launches the new agent into the *same* workspace and
    /// therefore tiles beside the focused pane, instead of opening in another workspace group.
    pub fn focused_pane_repo(&self) -> Option<String> {
        let fid = self.focused_pane.as_deref()?;
        let repo_id = self
            .data
            .tasks
            .iter()
            .find(|t| t.id == fid)?
            .repository
            .clone();
        self.data
            .workspaces
            .iter()
            .find(|w| w.id == repo_id)
            .map(|w| w.path.clone())
    }

    /// The repo a pane split should launch into: the focused pane's workspace, else the usual
    /// quick-launch target (SUM-136).
    pub fn split_target_repo(&self) -> String {
        self.focused_pane_repo()
            .unwrap_or_else(|| self.launch_target_repo())
    }

    /// Apply loaded data, clamping both section selections and snapping the AGENTS selection onto
    /// the nearest agent row (SUM-134).
    pub fn set_data(&mut self, data: Data) {
        self.data = data;
        let ws_len = self.ws_items().len();
        if self.ws_selected >= ws_len {
            self.ws_selected = ws_len.saturating_sub(1);
        }
        let nav_len = self.nav_items().len();
        if self.agent_selected >= nav_len {
            self.agent_selected = nav_len.saturating_sub(1);
        }
        self.snap_agent_to_selectable(1);
    }

    // ---- pane multiplexer ops (pure; SUM-132/134) ---------------------------------------------

    /// The workspace key a task belongs to (SUM-134): its `repository` id when that names a
    /// registered workspace, else the [`OTHER_WS_KEY`] sentinel. Mirrors [`Model::nav_items`]'
    /// grouping so the WORKSPACES panel and the pane groups stay in lockstep.
    pub fn ws_key(&self, task_id: &str) -> String {
        let repo = self
            .data
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.repository.clone());
        match repo {
            Some(r) if self.data.workspaces.iter().any(|w| w.id == r) => r,
            _ => OTHER_WS_KEY.to_string(),
        }
    }

    /// The pane group currently shown in the grid (SUM-134): [`Model::active_ws`]'s tree, if open.
    pub fn active_panes(&self) -> Option<&PaneLayout> {
        self.active_ws.as_ref().and_then(|k| self.panes.get(k))
    }

    /// Open `id` as a pane in its workspace group and focus it (SUM-132/134). Makes that group the
    /// active one. The group's first pane fills the grid; subsequent panes split the currently
    /// focused pane using `split_orient`. Re-opening an already-open pane just focuses it. Focus
    /// moves to the pane grid.
    pub fn open_pane(&mut self, id: &str) {
        self.focus = Focus::Panes;
        self.zoomed = false;
        let key = self.ws_key(id);
        let split_orient = self.split_orient;
        let prev_focus = self.focused_pane.clone();
        match self.panes.get_mut(&key) {
            None => {
                self.panes
                    .insert(key.clone(), PaneLayout::Leaf(id.to_string()));
            }
            Some(layout) => {
                if !layout.contains(id) {
                    let target = prev_focus
                        .filter(|f| layout.contains(f))
                        .or_else(|| layout.leaves().last().cloned());
                    if let Some(target) = target {
                        layout.split_leaf(&target, id, split_orient);
                    }
                }
            }
        }
        self.active_ws = Some(key);
        self.focused_pane = Some(id.to_string());
    }

    /// Close the pane for `id` from its group (SUM-132/134). Re-focuses a surviving sibling in the
    /// active group; drops the group's key when it empties; returns focus to the sidebar when no
    /// groups remain. Also drops its cached screen.
    pub fn close_pane(&mut self, id: &str) {
        self.pane_screens.remove(id);
        let key = self.ws_key(id);
        if let Some(layout) = self.panes.remove(&key) {
            if let Some(next) = layout.without(id) {
                self.panes.insert(key, next);
            }
        }
        // Refocus within the active group, else fall back to any remaining group.
        let active = self.active_ws.clone().and_then(|k| self.panes.get(&k));
        match active {
            Some(layout) => {
                let focus_ok = self
                    .focused_pane
                    .as_deref()
                    .map(|f| layout.contains(f))
                    .unwrap_or(false);
                if !focus_ok {
                    self.focused_pane = layout.leaves().first().cloned();
                }
            }
            None => {
                if let Some((k, layout)) = self.panes.iter().next() {
                    self.active_ws = Some(k.clone());
                    self.focused_pane = layout.leaves().first().cloned();
                } else {
                    self.active_ws = None;
                    self.focused_pane = None;
                    self.focus = Focus::Sidebar;
                    self.zoomed = false;
                }
            }
        }
    }

    /// The focused pane id, if any is open.
    pub fn focused_pane(&self) -> Option<&str> {
        self.focused_pane.as_deref()
    }

    /// Toggle zoom of the focused pane (fills the whole grid area).
    pub fn toggle_zoom(&mut self) {
        if self.focused_pane.is_some() {
            self.zoomed = !self.zoomed;
        }
    }

    /// Resize the focused pane's governing split by moving its boundary toward `dir` (SUM-135):
    /// Right/Down grow child `a`, Left/Up shrink it, by 5% (clamped to `10..=90`).
    pub fn resize_focused(&mut self, dir: Dir) {
        let Some(focused) = self.focused_pane.clone() else {
            return;
        };
        let Some(layout) = self.active_ws.clone().and_then(|k| self.panes.get_mut(&k)) else {
            return;
        };
        let axis_cols = matches!(dir, Dir::Left | Dir::Right);
        let delta = match dir {
            Dir::Right | Dir::Down => 5,
            Dir::Left | Dir::Up => -5,
        };
        layout.resize(&focused, axis_cols, delta);
    }

    /// Move pane focus spatially within `area` (the grid rect). Picks the nearest leaf whose
    /// centre lies in `dir` from the focused pane's centre, within the active group.
    pub fn focus_dir(&mut self, dir: Dir, area: Rect) {
        let Some(layout) = self.active_panes() else {
            return;
        };
        let rects = layout.leaf_rects(area);
        let Some(cur) = self
            .focused_pane
            .as_ref()
            .and_then(|f| rects.iter().find(|(id, _)| id == f))
            .map(|(_, r)| *r)
        else {
            return;
        };
        let (cx, cy) = (
            cur.x as i32 + cur.width as i32 / 2,
            cur.y as i32 + cur.height as i32 / 2,
        );
        let mut best: Option<(i32, String)> = None;
        for (id, r) in &rects {
            if Some(id.as_str()) == self.focused_pane.as_deref() {
                continue;
            }
            let (rx, ry) = (
                r.x as i32 + r.width as i32 / 2,
                r.y as i32 + r.height as i32 / 2,
            );
            let ok = match dir {
                Dir::Left => rx < cx,
                Dir::Right => rx > cx,
                Dir::Up => ry < cy,
                Dir::Down => ry > cy,
            };
            if !ok {
                continue;
            }
            let dist = (rx - cx).pow(2) + (ry - cy).pow(2);
            if best.as_ref().map(|(d, _)| dist < *d).unwrap_or(true) {
                best = Some((dist, id.clone()));
            }
        }
        if let Some((_, id)) = best {
            self.focused_pane = Some(id);
        }
    }

    /// Split the sidebar `area` into the stacked WORKSPACES (top) and AGENTS (bottom) panels
    /// (SUM-134). The WORKSPACES panel is sized to its rows plus a top/bottom border, capped at
    /// half the sidebar so the AGENTS panel always gets room. Shared by render + mouse hit-testing.
    pub fn sidebar_split(&self, area: Rect) -> (Rect, Rect) {
        let ws_rows = self.ws_items().len() as u16;
        let cap = area.height.saturating_sub(3).max(3);
        let ws_h = (ws_rows + 2)
            .clamp(3, area.height.saturating_sub(3).max(3))
            .min(cap);
        let top = Rect {
            height: ws_h.min(area.height),
            ..area
        };
        let bottom = Rect {
            y: area.y + top.height,
            height: area.height.saturating_sub(top.height),
            ..area
        };
        (top, bottom)
    }

    /// Left-click hit-test for the sidebar (SUM-133/134): map `(col,row)` to a row in whichever
    /// stacked panel it lands in, select it, focus that section + the sidebar. Returns whether it
    /// hit. A click in AGENTS on a header snaps to the nearest agent.
    pub fn select_nav_at(&mut self, col: u16, row: u16, area: Rect) -> bool {
        if col < area.x || col >= area.x + area.width {
            return false;
        }
        let (top, bottom) = self.sidebar_split(area);
        // WORKSPACES panel.
        if row > top.y && row + 1 < top.y + top.height {
            let idx = (row - top.y - 1) as usize;
            if idx >= self.ws_items().len() {
                return false;
            }
            self.ws_selected = idx;
            self.sidebar = SidebarSection::Workspaces;
            self.focus = Focus::Sidebar;
            self.sync_active_ws();
            return true;
        }
        // AGENTS panel.
        if row > bottom.y && row + 1 < bottom.y + bottom.height {
            let idx = (row - bottom.y - 1) as usize;
            if idx >= self.nav_items().len() {
                return false;
            }
            self.agent_selected = idx;
            self.sidebar = SidebarSection::Agents;
            self.focus = Focus::Sidebar;
            self.snap_agent_to_selectable(1);
            return true;
        }
        false
    }

    /// Left-click hit-test for the pane grid (SUM-133): focus the pane whose rect contains
    /// `(col,row)` within `area`. Returns whether it hit a pane in the active group.
    pub fn focus_pane_at(&mut self, col: u16, row: u16, area: Rect) -> bool {
        let Some(layout) = self.active_panes() else {
            return false;
        };
        for (id, r) in layout.leaf_rects(area) {
            if col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height {
                self.focused_pane = Some(id);
                self.focus = Focus::Panes;
                return true;
            }
        }
        false
    }
}

/// The pure state transition. Returns effects the runtime should execute.
pub fn update(model: &mut Model, key: Key) -> Vec<Effect> {
    match model.view {
        View::NewTask => return update_form(model, key),
        View::Launch => return update_launch(model, key),
        View::Confirm => return update_confirm(model, key),
        View::OpenFolder => return update_open_folder(model, key),
        View::Help => {
            if matches!(key, Key::Esc | Key::Char('q') | Key::Char('?')) {
                model.view = View::Home;
            }
            return Vec::new();
        }
        View::Home => {}
    }

    match key {
        Key::Quit | Key::Char('q') => model.should_quit = true,
        Key::Char('?') => model.view = View::Help,
        // Browse for a folder to register as a workspace (SUM-130): open the browser at the
        // launch directory.
        Key::Char('o') => {
            let start = model
                .cwd
                .clone()
                .or_else(|| model.cwd_repo.clone())
                .unwrap_or_else(|| ".".to_string());
            model.view = View::OpenFolder;
            return vec![Effect::ListDir(start)];
        }
        // One-key quick-launch palette (SUM-130): pick any agent or a shell.
        Key::Char('c') => open_launch(model, model.launch_target_repo()),
        Key::Char('n') => {
            model.form = NewTaskForm {
                repo: model.default_repo(),
                ..NewTaskForm::default()
            };
            model.view = View::NewTask;
        }
        // Close (SUM-130): terminate a running agent or forget a dead one, via a confirm prompt.
        // Acts on the AGENTS selection regardless of which section is focused.
        Key::Char('x') => {
            if let Some(NavItem::Agent(ti)) = model.selected_nav() {
                if let Some(t) = model.data.tasks.get(ti) {
                    let id = t.id.clone();
                    let pending = if is_terminal_state(&t.state) {
                        PendingAction::Forget(id)
                    } else {
                        PendingAction::Terminate(id)
                    };
                    model.pending = Some(pending);
                    model.view = View::Confirm;
                }
            }
        }
        // Toggle the focused sidebar section (SUM-134). `BackTab` maps to `Key::Tab` upstream too,
        // so a single arm covers both directions of a two-section toggle.
        Key::Tab => model.toggle_section(),
        Key::Up | Key::Char('k') => model.move_selection(-1),
        Key::Down | Key::Char('j') => model.move_selection(1),
        Key::Enter => return activate_selection(model),
        _ => {}
    }
    Vec::new()
}

/// Act on the focused sidebar section (SUM-134): in AGENTS open the selected agent as a pane
/// (start-or-restart + attach); in WORKSPACES open the quick-launch palette into that workspace.
fn activate_selection(model: &mut Model) -> Vec<Effect> {
    match model.sidebar {
        SidebarSection::Agents => {
            if let Some(NavItem::Agent(ti)) = model.selected_nav() {
                if let Some(t) = model.data.tasks.get(ti) {
                    let (id, state) = (t.id.clone(), t.state.clone());
                    if is_terminated_state(&state) {
                        model.status = "agent terminated — press x to remove".to_string();
                    } else {
                        // Open the agent as a pane and focus it (SUM-132). The runtime
                        // starts/restarts the provider and wires the live Attach session.
                        model.open_pane(&id);
                        return vec![Effect::OpenPane(id)];
                    }
                }
            }
        }
        SidebarSection::Workspaces => open_launch(model, model.default_repo()),
    }
    Vec::new()
}

/// Whether a state string is terminal (cannot be restarted; only forgotten).
fn is_terminated_state(state: &str) -> bool {
    state == "TERMINATED"
}

/// Whether an agent in this state should be *forgotten* rather than *terminated* by `x` — i.e. it
/// is already terminal with no live provider (SUM-130).
fn is_terminal_state(state: &str) -> bool {
    matches!(state, "TERMINATED" | "FAILED")
}

/// Open the quick-launch palette targeting `repo` (SUM-130).
fn open_launch(model: &mut Model, repo: String) {
    model.launch_repo = repo;
    model.launch_selected = 0;
    model.view = View::Launch;
}

/// Keys for the quick-launch palette (SUM-130): number keys pick directly; ↑/↓ + Enter also work.
fn update_launch(model: &mut Model, key: Key) -> Vec<Effect> {
    let pick = |model: &mut Model, idx: usize| -> Vec<Effect> {
        let (_, provider, shell) = LAUNCH_ITEMS[idx];
        let repo = model.launch_repo.clone();
        if repo.trim().is_empty() {
            model.status = "no target folder — open one with 'o' first".to_string();
            model.view = View::Home;
            return Vec::new();
        }
        model.view = View::Home;
        vec![Effect::QuickLaunch {
            provider: provider.to_string(),
            repo,
            shell,
        }]
    };
    match key {
        Key::Esc => model.view = View::Home,
        Key::Up | Key::Char('k') => {
            model.launch_selected = model.launch_selected.saturating_sub(1);
        }
        Key::Down | Key::Char('j') => {
            model.launch_selected = (model.launch_selected + 1).min(LAUNCH_ITEMS.len() - 1);
        }
        Key::Char(c @ '1'..='5') => {
            let idx = (c as usize) - ('1' as usize);
            if idx < LAUNCH_ITEMS.len() {
                return pick(model, idx);
            }
        }
        Key::Enter => return pick(model, model.launch_selected),
        _ => {}
    }
    Vec::new()
}

/// Keys for the destructive-action confirmation prompt (SUM-130).
fn update_confirm(model: &mut Model, key: Key) -> Vec<Effect> {
    match key {
        Key::Esc | Key::Char('n') => {
            model.pending = None;
            model.view = View::Home;
        }
        Key::Enter | Key::Char('y') => {
            model.view = View::Home;
            if let Some(action) = model.pending.take() {
                let effect = match action {
                    PendingAction::Terminate(id) => Effect::TerminateTask(id),
                    PendingAction::Forget(id) => Effect::ForgetTask(id),
                };
                return vec![effect, Effect::Refresh];
            }
        }
        _ => {}
    }
    Vec::new()
}

/// Keys for the interactive folder browser (SUM-130). Filesystem listing is done by the runtime
/// via [`Effect::ListDir`]; this pure handler only computes the next path to list and emits the
/// register action. Entries are directory names within `browse_dir`, with a leading `..` unless at
/// the filesystem root.
fn update_open_folder(model: &mut Model, key: Key) -> Vec<Effect> {
    match key {
        Key::Esc => model.view = View::Home,
        Key::Up | Key::Char('k') => {
            model.browse_selected = model.browse_selected.saturating_sub(1);
        }
        Key::Down | Key::Char('j') => {
            let max = model.browse_entries.len().saturating_sub(1);
            model.browse_selected = (model.browse_selected + 1).min(max);
        }
        // Descend into (or ascend out of) the highlighted directory.
        Key::Enter | Key::Right | Key::Char('l') => {
            if let Some(entry) = model.browse_entries.get(model.browse_selected).cloned() {
                let target = join_browse(&model.browse_dir, &entry);
                return vec![Effect::ListDir(target)];
            }
        }
        // Ascend to the parent directory.
        Key::Left | Key::Char('h') | Key::Backspace => {
            let target = join_browse(&model.browse_dir, "..");
            return vec![Effect::ListDir(target)];
        }
        // Register the directory currently being browsed as a workspace.
        Key::Char('a') => {
            let path = model.browse_dir.clone();
            if path.is_empty() {
                return Vec::new();
            }
            model.status = format!("adding {path}");
            model.view = View::Home;
            return vec![Effect::AddWorkspace(path), Effect::Refresh];
        }
        _ => {}
    }
    Vec::new()
}

/// Resolve a browser navigation target: `dir` joined with `entry` (`..` ascends), normalized.
/// Pure path arithmetic (no filesystem access) so it stays unit-testable.
fn join_browse(dir: &str, entry: &str) -> String {
    use std::path::Path;
    let base = if dir.is_empty() {
        Path::new(".")
    } else {
        Path::new(dir)
    };
    let joined = if entry == ".." {
        base.parent().unwrap_or(base).to_path_buf()
    } else {
        base.join(entry)
    };
    joined.to_string_lossy().into_owned()
}

fn update_form(model: &mut Model, key: Key) -> Vec<Effect> {
    match key {
        Key::Esc => model.view = View::Home,
        Key::Tab | Key::Down => model.form.field = next_field(model.form.field),
        Key::Up => model.form.field = prev_field(model.form.field),
        Key::Left if model.form.field == FormField::Provider => {
            if model.form.provider_idx == 0 {
                model.form.provider_idx = PROVIDERS.len() - 1;
            } else {
                model.form.provider_idx -= 1;
            }
        }
        Key::Right if model.form.field == FormField::Provider => {
            model.form.provider_idx = (model.form.provider_idx + 1) % PROVIDERS.len();
        }
        Key::Backspace => {
            if let Some(buf) = field_buf(model) {
                buf.pop();
            }
        }
        Key::Char(c) => {
            if let Some(buf) = field_buf(model) {
                buf.push(c);
            }
        }
        Key::Enter => {
            if let Some(req) = model.form.to_request() {
                model.status = "creating agent".to_string();
                model.view = View::Home;
                return vec![Effect::CreateTask(req), Effect::Refresh];
            }
            model.status = "a repository is required".to_string();
        }
        _ => {}
    }
    Vec::new()
}

fn field_buf(model: &mut Model) -> Option<&mut String> {
    match model.form.field {
        FormField::Title => Some(&mut model.form.title),
        FormField::Repo => Some(&mut model.form.repo),
        FormField::Base => Some(&mut model.form.base),
        FormField::Provider => None,
    }
}

fn next_field(f: FormField) -> FormField {
    match f {
        FormField::Title => FormField::Repo,
        FormField::Repo => FormField::Provider,
        FormField::Provider => FormField::Base,
        FormField::Base => FormField::Title,
    }
}

fn prev_field(f: FormField) -> FormField {
    match f {
        FormField::Title => FormField::Base,
        FormField::Repo => FormField::Title,
        FormField::Provider => FormField::Repo,
        FormField::Base => FormField::Provider,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, repo: &str) -> TaskView {
        TaskView {
            id: id.into(),
            title: format!("title {id}"),
            provider: "codex".into(),
            state: "ACTIVE".into(),
            repository: repo.into(),
            base_branch: "main".into(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn workspace(id: &str, path: &str) -> WorkspaceView {
        WorkspaceView {
            id: id.into(),
            path: path.into(),
            name: id.into(),
            created_at_ms: 0,
            task_count: 0,
        }
    }

    fn model_with_data() -> Model {
        let mut m = Model::default();
        m.set_data(Data {
            workspaces: vec![workspace("repo_a", "/src/a"), workspace("repo_b", "/src/b")],
            tasks: vec![
                task("t1", "repo_a"),
                task("t2", "repo_a"),
                task("t3", "repo_b"),
            ],
            ..Default::default()
        });
        m
    }

    #[test]
    fn nav_items_group_agents_under_their_workspace() {
        let m = model_with_data();
        let nav = m.nav_items();
        // repo_a header, t1, t2, repo_b header, t3
        assert_eq!(
            nav,
            vec![
                NavItem::Workspace(0),
                NavItem::Agent(0),
                NavItem::Agent(1),
                NavItem::Workspace(1),
                NavItem::Agent(2),
            ]
        );
    }

    #[test]
    fn ws_items_list_only_workspaces_and_add_other_when_ungrouped() {
        // With every agent grouped, there is no Other row.
        let m = model_with_data();
        assert_eq!(m.ws_items(), vec![WsRow::Workspace(0), WsRow::Workspace(1)]);

        // An ungrouped agent adds a trailing Other row.
        let mut m = Model::default();
        m.set_data(Data {
            workspaces: vec![workspace("repo_a", "/src/a")],
            tasks: vec![task("t1", "repo_a"), task("t2", "repo_x")],
            ..Default::default()
        });
        assert_eq!(m.ws_items(), vec![WsRow::Workspace(0), WsRow::Other]);
    }

    #[test]
    fn tab_toggles_the_focused_sidebar_section() {
        let mut m = model_with_data();
        assert_eq!(m.sidebar, SidebarSection::Workspaces);
        update(&mut m, Key::Tab);
        assert_eq!(m.sidebar, SidebarSection::Agents);
        update(&mut m, Key::Tab);
        assert_eq!(m.sidebar, SidebarSection::Workspaces);
    }

    #[test]
    fn j_k_move_within_the_focused_section_only() {
        let mut m = model_with_data();
        // WORKSPACES focused: moves the ws selection, not the agent selection.
        assert_eq!(m.selected_ws(), Some(WsRow::Workspace(0)));
        update(&mut m, Key::Char('j'));
        assert_eq!(m.selected_ws(), Some(WsRow::Workspace(1)));
        assert_eq!(m.agent_selected, 1, "agent selection untouched");

        // Switch to AGENTS: j/k step over headers onto agents.
        update(&mut m, Key::Tab);
        assert_eq!(m.selected_nav(), Some(NavItem::Agent(0)));
        update(&mut m, Key::Char('j'));
        assert_eq!(m.selected_nav(), Some(NavItem::Agent(1)));
        update(&mut m, Key::Char('j')); // skips the repo_b header onto t3
        assert_eq!(m.selected_nav(), Some(NavItem::Agent(2)));
    }

    #[test]
    fn ungrouped_agents_land_under_an_unregistered_header() {
        let mut m = Model::default();
        m.set_data(Data {
            workspaces: vec![],
            tasks: vec![task("t1", "repo_x")],
            ..Default::default()
        });
        assert_eq!(
            m.nav_items(),
            vec![NavItem::Unregistered, NavItem::Agent(0)]
        );
    }

    #[test]
    fn no_numbered_tab_switching() {
        // Number keys are no longer view switches; they do nothing in Home.
        let mut m = model_with_data();
        for c in ['1', '2', '3', '4', '5'] {
            update(&mut m, Key::Char(c));
            assert_eq!(m.view, View::Home);
        }
    }

    /// The `agent_selected` index of the AGENTS row that is `NavItem::Agent(ti)`.
    fn agent_row(m: &Model, ti: usize) -> usize {
        m.nav_items()
            .iter()
            .position(|n| *n == NavItem::Agent(ti))
            .unwrap()
    }

    #[test]
    fn enter_on_agent_opens_it_as_a_focused_pane() {
        let mut m = model_with_data();
        m.sidebar = SidebarSection::Agents;
        m.agent_selected = agent_row(&m, 0); // Agent(0) == t1
        let effects = update(&mut m, Key::Enter);
        // Enter opens the agent as a pane (SUM-132): pure layout mutated + OpenPane for the runtime.
        assert_eq!(effects, vec![Effect::OpenPane("t1".into())]);
        assert_eq!(m.focus, Focus::Panes);
        assert_eq!(m.focused_pane(), Some("t1"));
        // t1 lives in the repo_a group.
        assert_eq!(m.active_ws.as_deref(), Some("repo_a"));
        assert_eq!(m.active_panes(), Some(&PaneLayout::Leaf("t1".into())));
    }

    #[test]
    fn opening_a_second_agent_in_the_same_workspace_splits_and_focuses_it() {
        let mut m = model_with_data();
        m.open_pane("t1"); // repo_a
        m.split_orient = Orient::Cols;
        m.open_pane("t2"); // repo_a
        assert_eq!(m.focused_pane(), Some("t2"));
        assert_eq!(m.active_panes().unwrap().leaves(), vec!["t1", "t2"]);
        // Only one group, holding both agents.
        assert_eq!(m.panes.len(), 1);
        // Re-opening an already-open pane just refocuses (no duplicate leaf).
        m.open_pane("t1");
        assert_eq!(m.focused_pane(), Some("t1"));
        assert_eq!(m.active_panes().unwrap().leaves(), vec!["t1", "t2"]);
    }

    #[test]
    fn opening_agents_in_two_workspaces_makes_two_isolated_groups() {
        let mut m = model_with_data();
        m.open_pane("t1"); // repo_a
        m.open_pane("t3"); // repo_b
        assert_eq!(m.panes.len(), 2);
        assert_eq!(m.panes.get("repo_a").unwrap().leaves(), vec!["t1"]);
        assert_eq!(m.panes.get("repo_b").unwrap().leaves(), vec!["t3"]);
        // Opening t3 made repo_b active and focused it.
        assert_eq!(m.active_ws.as_deref(), Some("repo_b"));
        assert_eq!(m.focused_pane(), Some("t3"));
        assert_eq!(m.active_panes().unwrap().leaves(), vec!["t3"]);
    }

    #[test]
    fn split_launch_targets_the_focused_panes_workspace() {
        // Regression (SUM-136): splitting from a focused pane must launch into that pane's
        // workspace so the new agent tiles beside it, not open in another workspace group.
        let mut m = model_with_data(); // repo_a @ /src/a (t1,t2), repo_b @ /src/b (t3)
        m.open_pane("t1"); // active_ws = repo_a
        assert_eq!(m.focused_pane_repo().as_deref(), Some("/src/a"));
        assert_eq!(m.split_target_repo(), "/src/a");
        // Launching another agent in the same workspace joins the active group (a real split).
        m.open_pane("t2"); // t2 is also repo_a
        assert_eq!(m.active_ws.as_deref(), Some("repo_a"));
        assert_eq!(m.active_panes().unwrap().leaves(), vec!["t1", "t2"]);
        assert_eq!(m.panes.len(), 1, "no new workspace group was created");
    }

    #[test]
    fn unregistered_agent_lands_in_the_other_group() {
        let mut m = Model::default();
        m.set_data(Data {
            workspaces: vec![workspace("repo_a", "/src/a")],
            tasks: vec![task("t1", "repo_a"), task("t2", "repo_x")],
            ..Default::default()
        });
        assert_eq!(m.ws_key("t2"), OTHER_WS_KEY);
        m.open_pane("t2");
        assert_eq!(m.active_ws.as_deref(), Some(OTHER_WS_KEY));
        assert_eq!(m.panes.get(OTHER_WS_KEY).unwrap().leaves(), vec!["t2"]);
    }

    #[test]
    fn closing_a_pane_collapses_the_split_and_refocuses() {
        let mut m = model_with_data();
        m.open_pane("t1");
        m.open_pane("t2");
        m.close_pane("t2");
        assert_eq!(m.active_panes(), Some(&PaneLayout::Leaf("t1".into())));
        assert_eq!(m.focused_pane(), Some("t1"));
        assert_eq!(m.focus, Focus::Panes);
        // Closing the last pane drops the group key and returns focus to the sidebar.
        m.close_pane("t1");
        assert!(m.panes.is_empty());
        assert_eq!(m.active_ws, None);
        assert_eq!(m.focus, Focus::Sidebar);
        assert_eq!(m.focused_pane(), None);
    }

    #[test]
    fn closing_a_groups_last_pane_drops_only_that_key() {
        let mut m = model_with_data();
        m.open_pane("t1"); // repo_a
        m.open_pane("t3"); // repo_b (now active)
        m.close_pane("t3");
        // repo_b's key is gone; repo_a survives and becomes the fallback active group.
        assert!(!m.panes.contains_key("repo_b"));
        assert!(m.panes.contains_key("repo_a"));
        assert_eq!(m.active_ws.as_deref(), Some("repo_a"));
        assert_eq!(m.focused_pane(), Some("t1"));
        assert_eq!(m.focus, Focus::Panes);
    }

    #[test]
    fn active_ws_and_active_panes_follow_workspace_selection() {
        let mut m = model_with_data();
        m.open_pane("t1"); // repo_a
        m.open_pane("t3"); // repo_b, now active
        assert_eq!(m.active_ws.as_deref(), Some("repo_b"));
        // Select repo_a in the WORKSPACES panel: the grid + focus follow to that group.
        m.sidebar = SidebarSection::Workspaces;
        m.ws_selected = 0; // repo_a
        m.sync_active_ws();
        assert_eq!(m.active_ws.as_deref(), Some("repo_a"));
        assert_eq!(m.focused_pane(), Some("t1"));
        assert_eq!(m.active_panes().unwrap().leaves(), vec!["t1"]);
        // A WORKSPACES j/k move re-points the active group too.
        update(&mut m, Key::Char('j')); // -> repo_b
        assert_eq!(m.active_ws.as_deref(), Some("repo_b"));
        assert_eq!(m.focused_pane(), Some("t3"));
    }

    #[test]
    fn focus_dir_moves_spatially_across_a_column_split() {
        use ratatui::layout::Rect;
        let mut m = model_with_data();
        m.open_pane("t1");
        m.split_orient = Orient::Cols;
        m.open_pane("t2"); // t1 | t2, focus t2
        let area = Rect::new(0, 0, 80, 24);
        m.focus_dir(Dir::Left, area);
        assert_eq!(m.focused_pane(), Some("t1"));
        m.focus_dir(Dir::Right, area);
        assert_eq!(m.focused_pane(), Some("t2"));
    }

    #[test]
    fn zoom_toggles_only_when_a_pane_is_open() {
        let mut m = model_with_data();
        m.toggle_zoom();
        assert!(!m.zoomed, "no pane → no zoom");
        m.open_pane("t1");
        m.toggle_zoom();
        assert!(m.zoomed);
        m.toggle_zoom();
        assert!(!m.zoomed);
    }

    #[test]
    fn splits_default_to_50_50_and_resize_adjusts_the_ratio() {
        use ratatui::layout::Rect;
        let mut m = model_with_data();
        m.open_pane("t1");
        m.split_orient = Orient::Cols;
        m.open_pane("t2"); // t1 | t2, focus t2
        let area = Rect::new(0, 0, 100, 24);
        // Default 50/50 → t1 gets the left 50 cols.
        let rects = m.active_panes().unwrap().leaf_rects(area);
        let t1 = rects.iter().find(|(id, _)| id == "t1").unwrap().1;
        assert_eq!(t1.width, 50);
        // Move the boundary left twice (−5 each) → child a (t1) shrinks to 40%.
        m.resize_focused(Dir::Left);
        m.resize_focused(Dir::Left);
        let rects = m.active_panes().unwrap().leaf_rects(area);
        let t1 = rects.iter().find(|(id, _)| id == "t1").unwrap().1;
        assert_eq!(t1.width, 40);
    }

    #[test]
    fn resize_clamps_and_ignores_the_wrong_axis() {
        use ratatui::layout::Rect;
        let mut m = model_with_data();
        m.open_pane("t1");
        m.split_orient = Orient::Cols;
        m.open_pane("t2");
        let area = Rect::new(0, 0, 100, 24);
        // Vertical resize on a purely horizontal (Cols) split is a no-op.
        m.resize_focused(Dir::Down);
        let t1 = m
            .active_panes()
            .unwrap()
            .leaf_rects(area)
            .into_iter()
            .find(|(id, _)| id == "t1")
            .unwrap()
            .1;
        assert_eq!(t1.width, 50);
        // Growing child a past the cap clamps at 90%.
        for _ in 0..20 {
            m.resize_focused(Dir::Right);
        }
        let t1 = m
            .active_panes()
            .unwrap()
            .leaf_rects(area)
            .into_iter()
            .find(|(id, _)| id == "t1")
            .unwrap()
            .1;
        assert_eq!(t1.width, 90);
    }

    #[test]
    fn prefix_parses_and_formats() {
        assert_eq!(
            Prefix::default(),
            Prefix {
                ctrl: true,
                ch: 'b'
            }
        );
        for s in ["ctrl-a", "ctrl+a", "c-a", "  Ctrl-A  "] {
            assert_eq!(
                Prefix::parse(s),
                Some(Prefix {
                    ctrl: true,
                    ch: 'a'
                })
            );
        }
        assert_eq!(
            Prefix::parse("b"),
            Some(Prefix {
                ctrl: false,
                ch: 'b'
            })
        );
        assert_eq!(Prefix::parse("ctrl-1"), None);
        assert_eq!(Prefix::parse("ctrl-ab"), None);
        assert_eq!(Prefix::parse(""), None);
        assert_eq!(
            Prefix {
                ctrl: true,
                ch: 'b'
            }
            .label(),
            "Ctrl-b"
        );
        assert_eq!(
            Prefix {
                ctrl: true,
                ch: 'b'
            }
            .literal_byte(),
            0x02
        );
        assert_eq!(
            Prefix {
                ctrl: true,
                ch: 'a'
            }
            .literal_byte(),
            0x01
        );
    }

    #[test]
    fn mouse_click_focuses_a_pane_and_selects_a_sidebar_row() {
        use ratatui::layout::Rect;
        let mut m = model_with_data();
        m.open_pane("t1"); // repo_a
        m.split_orient = Orient::Cols;
        m.open_pane("t2"); // repo_a: t1 | t2
        let grid = Rect::new(30, 1, 60, 22);
        // A click in the right half hits t2, the left half hits t1 (within the active group).
        assert!(m.focus_pane_at(80, 10, grid));
        assert_eq!(m.focused_pane(), Some("t2"));
        assert!(m.focus_pane_at(35, 10, grid));
        assert_eq!(m.focused_pane(), Some("t1"));

        // Sidebar hit-testing splits the rect into WORKSPACES (top) / AGENTS (bottom).
        let sidebar = Rect::new(0, 1, 30, 22);
        let (top, bottom) = m.sidebar_split(sidebar);
        // A WORKSPACES click selects a workspace row and focuses that section.
        assert!(m.select_nav_at(5, top.y + 2, sidebar)); // idx (row-top.y-1)=1 → Workspace(1)
        assert_eq!(m.selected_ws(), Some(WsRow::Workspace(1)));
        assert_eq!(m.sidebar, SidebarSection::Workspaces);
        assert_eq!(m.focus, Focus::Sidebar);
        // Selecting repo_b made it the active group.
        assert_eq!(m.active_ws.as_deref(), Some("repo_b"));

        // An AGENTS click selects an agent row and focuses that section.
        // nav_items() = [Workspace(0), Agent(0)=t1, Agent(1)=t2, Workspace(1), Agent(2)=t3]
        assert!(m.select_nav_at(5, bottom.y + 2, sidebar)); // idx 1 → Agent(0)
        assert_eq!(m.selected_nav(), Some(NavItem::Agent(0)));
        assert_eq!(m.sidebar, SidebarSection::Agents);
        assert_eq!(m.focus, Focus::Sidebar);
        // A click on the WORKSPACES top border doesn't select.
        assert!(!m.select_nav_at(5, top.y, sidebar));
    }

    #[test]
    fn enter_on_a_terminated_agent_does_not_attach() {
        let mut m = model_with_data();
        m.data.tasks[0].state = "TERMINATED".into();
        m.sidebar = SidebarSection::Agents;
        m.agent_selected = agent_row(&m, 0); // Agent(0) == t1
        let effects = update(&mut m, Key::Enter);
        assert!(effects.is_empty(), "must not attach to a terminated agent");
        assert!(m.status.contains("terminated"));
    }

    #[test]
    fn enter_on_workspace_opens_launch_palette_prefilled() {
        let mut m = model_with_data(); // WORKSPACES focused by default
        m.ws_selected = 0; // Workspace(0) == repo_a @ /src/a
        update(&mut m, Key::Enter);
        assert_eq!(m.view, View::Launch);
        assert_eq!(m.launch_repo, "/src/a");
    }

    #[test]
    fn c_opens_launch_palette_and_picks_an_agent() {
        let mut m = model_with_data();
        m.ws_selected = 1; // Workspace(1) == repo_b @ /src/b
        update(&mut m, Key::Char('c'));
        assert_eq!(m.view, View::Launch);
        assert_eq!(m.launch_repo, "/src/b");
        // '2' picks codex and quick-launches it into that repo.
        let effects = update(&mut m, Key::Char('2'));
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::QuickLaunch { provider, repo, shell }
                if provider == "codex" && repo == "/src/b" && !*shell
        )));
        assert_eq!(m.view, View::Home);
    }

    #[test]
    fn launch_palette_shell_entry_requests_a_shell_anywhere() {
        let mut m = Model {
            cwd: Some("/tmp/anywhere".into()),
            ..Model::default()
        };
        update(&mut m, Key::Char('c')); // no selection/workspace → falls back to cwd
        assert_eq!(m.launch_repo, "/tmp/anywhere");
        let effects = update(&mut m, Key::Char('5')); // shell entry
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::QuickLaunch { provider, repo, shell }
                if provider == "generic" && repo == "/tmp/anywhere" && *shell
        )));
    }

    #[test]
    fn x_terminates_a_running_agent_after_confirm() {
        let mut m = model_with_data(); // t1 is ACTIVE
        m.sidebar = SidebarSection::Agents;
        m.agent_selected = agent_row(&m, 0); // Agent(0) == t1
        update(&mut m, Key::Char('x'));
        assert_eq!(m.view, View::Confirm);
        assert_eq!(m.pending, Some(PendingAction::Terminate("t1".into())));
        let effects = update(&mut m, Key::Enter);
        assert!(effects.contains(&Effect::TerminateTask("t1".into())));
        assert!(effects.contains(&Effect::Refresh));
        assert!(m.pending.is_none());
    }

    #[test]
    fn x_forgets_a_terminal_agent_and_esc_cancels() {
        let mut m = model_with_data();
        m.data.tasks[0].state = "FAILED".into();
        m.sidebar = SidebarSection::Agents;
        m.agent_selected = agent_row(&m, 0); // Agent(0) == t1
        update(&mut m, Key::Char('x'));
        assert_eq!(m.pending, Some(PendingAction::Forget("t1".into())));
        let effects = update(&mut m, Key::Esc);
        assert!(effects.is_empty());
        assert!(m.pending.is_none());
        assert_eq!(m.view, View::Home);
    }

    #[test]
    fn n_prefills_repo_from_selected_workspace() {
        let mut m = model_with_data(); // WORKSPACES focused
        m.ws_selected = 1; // Workspace(1) == repo_b @ /src/b
        update(&mut m, Key::Char('n'));
        assert_eq!(m.view, View::NewTask);
        assert_eq!(m.form.repo, "/src/b");
    }

    #[test]
    fn o_opens_folder_browser_and_a_adds_the_current_dir() {
        let mut m = Model {
            cwd: Some("/home/me".into()),
            ..Model::default()
        };
        let effects = update(&mut m, Key::Char('o'));
        assert_eq!(m.view, View::OpenFolder);
        assert!(effects.contains(&Effect::ListDir("/home/me".to_string())));

        // Simulate the runtime having listed the dir, then navigate + register.
        m.browse_dir = "/src".into();
        m.browse_entries = vec!["..".into(), "app".into()];
        update(&mut m, Key::Down); // -> "app"
        let effects = update(&mut m, Key::Enter);
        assert!(effects.contains(&Effect::ListDir("/src/app".to_string())));
        let effects = update(&mut m, Key::Char('a'));
        assert!(effects.contains(&Effect::AddWorkspace("/src".to_string())));
        assert_eq!(m.view, View::Home);
    }

    #[test]
    fn folder_browser_ascends_with_backspace() {
        let mut m = Model {
            view: View::OpenFolder,
            browse_dir: "/src/app".into(),
            browse_entries: vec!["..".into()],
            ..Model::default()
        };
        let effects = update(&mut m, Key::Backspace);
        assert!(effects.contains(&Effect::ListDir("/src".to_string())));
    }

    #[test]
    fn join_browse_descends_and_ascends() {
        assert_eq!(join_browse("/src", "app"), "/src/app");
        assert_eq!(join_browse("/src/app", ".."), "/src");
    }

    #[test]
    fn new_task_needs_only_a_repo_title_optional() {
        let form = NewTaskForm {
            repo: "/src/app".into(),
            ..NewTaskForm::default()
        };
        let req = form.to_request().expect("repo alone is enough");
        assert!(req.title.is_empty());
        assert!(NewTaskForm::default().to_request().is_none());
    }

    #[test]
    fn quit_and_help() {
        let mut m = Model::default();
        update(&mut m, Key::Char('?'));
        assert_eq!(m.view, View::Help);
        update(&mut m, Key::Esc);
        assert_eq!(m.view, View::Home);
        update(&mut m, Key::Char('q'));
        assert!(m.should_quit);
    }
}
