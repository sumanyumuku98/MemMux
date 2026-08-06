//! The Elm-architecture core of the TUI (SUM-81): a `Model`, an abstract `Key`, and a pure
//! `update` that returns the next model plus [`Effect`]s for the runtime to execute. Keeping
//! `update` free of terminal and socket I/O makes the whole interaction layer unit-testable.
//!
//! The UI is a single **Home** view (SUM-127): a sidebar of workspaces with their agents grouped
//! underneath, plus modals for creating a task / opening a folder / help. There are no numbered
//! tabs — navigation is the sidebar, and Enter on an agent drops into an interactive session.

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

/// A row in the sidebar's flattened workspace→agent navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavItem {
    /// A registered workspace header (index into [`Data::workspaces`]).
    Workspace(usize),
    /// Header for agents with no registered workspace.
    Unregistered,
    /// An agent (index into [`Data::tasks`]).
    Agent(usize),
}

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
    /// Open an agent (SUM-130): reuse a live provider, else start a queued one, else restart a
    /// dead/failed one — then attach if any of those yields a running provider. The runtime owns
    /// this fallback chain because only the daemon knows whether a provider is actually alive.
    OpenTask(String),
    /// Terminate a running/queued agent (SUM-130).
    TerminateTask(String),
    /// Forget a terminal agent, removing it from the registry (SUM-130).
    ForgetTask(String),
    /// List a directory for the workspace folder browser (SUM-130).
    ListDir(String),
    /// Register a folder as a workspace (SUM-124).
    AddWorkspace(String),
    /// Enter interactive raw passthrough for a task (SUM-125; runtime owns the terminal).
    Attach(String),
}

/// The full application model.
#[derive(Clone, Debug)]
pub struct Model {
    /// Current view.
    pub view: View,
    /// Loaded data.
    pub data: Data,
    /// Selected row index into the sidebar nav list.
    pub selected: usize,
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
            selected: 0,
            form: NewTaskForm::default(),
            cwd_repo: None,
            cwd: None,
            launch_selected: 0,
            launch_repo: String::new(),
            browse_dir: String::new(),
            browse_entries: Vec::new(),
            browse_selected: 0,
            pending: None,
            status: "connected".to_string(),
            should_quit: false,
        }
    }
}

impl Model {
    /// The flattened sidebar list: each registered workspace header followed by its agents, then
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

    /// The currently selected nav row, if any.
    pub fn selected_nav(&self) -> Option<NavItem> {
        self.nav_items().get(self.selected).copied()
    }

    /// Default repo path for a new task (SUM-119): the selected workspace (or the selected agent's
    /// workspace), else the launch directory's git root.
    pub fn default_repo(&self) -> String {
        match self.selected_nav() {
            Some(NavItem::Workspace(wi)) => self
                .data
                .workspaces
                .get(wi)
                .map(|w| w.path.clone())
                .unwrap_or_default(),
            Some(NavItem::Agent(ti)) => self
                .data
                .tasks
                .get(ti)
                .and_then(|t| self.data.workspaces.iter().find(|w| w.id == t.repository))
                .map(|w| w.path.clone())
                .or_else(|| self.cwd_repo.clone())
                .unwrap_or_default(),
            _ => self.cwd_repo.clone().unwrap_or_default(),
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

    /// Apply loaded data, clamping the selection against the nav list.
    pub fn set_data(&mut self, data: Data) {
        self.data = data;
        let len = self.nav_items().len();
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
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
        Key::Up | Key::Char('k') => move_selection(model, -1),
        Key::Down | Key::Char('j') => move_selection(model, 1),
        Key::Enter => return activate_selection(model),
        _ => {}
    }
    Vec::new()
}

/// Act on the selected sidebar row: open an agent (start-or-restart + attach), or open the
/// quick-launch palette into the selected workspace.
fn activate_selection(model: &mut Model) -> Vec<Effect> {
    match model.selected_nav() {
        Some(NavItem::Agent(ti)) => {
            if let Some(t) = model.data.tasks.get(ti) {
                let (id, state) = (t.id.clone(), t.state.clone());
                if is_terminated_state(&state) {
                    model.status = "agent terminated — press x to remove".to_string();
                } else {
                    // OpenTask both launches (start-or-restart) and, on success, drives the attach
                    // — so a dead agent no longer flashes an empty screen (SUM-130).
                    return vec![Effect::OpenTask(id)];
                }
            }
        }
        Some(NavItem::Workspace(_)) | Some(NavItem::Unregistered) | None => {
            open_launch(model, model.default_repo());
        }
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

fn move_selection(model: &mut Model, delta: isize) {
    let len = model.nav_items().len();
    if len == 0 {
        return;
    }
    let next = (model.selected as isize + delta).clamp(0, len as isize - 1);
    model.selected = next as usize;
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

    #[test]
    fn j_k_navigate_the_sidebar() {
        let mut m = model_with_data();
        assert_eq!(m.selected, 0); // repo_a header
        update(&mut m, Key::Char('j'));
        assert_eq!(m.selected_nav(), Some(NavItem::Agent(0)));
        update(&mut m, Key::Char('j'));
        assert_eq!(m.selected_nav(), Some(NavItem::Agent(1)));
    }

    #[test]
    fn enter_on_agent_opens_it() {
        let mut m = model_with_data();
        m.selected = 1; // Agent(0) == t1
        let effects = update(&mut m, Key::Enter);
        // OpenTask expands in the runtime to start-or-restart and attaches only if a live provider
        // results (SUM-130) — no separate unconditional Attach.
        assert_eq!(effects, vec![Effect::OpenTask("t1".into())]);
    }

    #[test]
    fn enter_on_a_terminated_agent_does_not_attach() {
        let mut m = model_with_data();
        m.data.tasks[0].state = "TERMINATED".into();
        m.selected = 1; // Agent(0) == t1
        let effects = update(&mut m, Key::Enter);
        assert!(effects.is_empty(), "must not attach to a terminated agent");
        assert!(m.status.contains("terminated"));
    }

    #[test]
    fn enter_on_workspace_opens_launch_palette_prefilled() {
        let mut m = model_with_data();
        m.selected = 0; // Workspace(0) == repo_a @ /src/a
        update(&mut m, Key::Enter);
        assert_eq!(m.view, View::Launch);
        assert_eq!(m.launch_repo, "/src/a");
    }

    #[test]
    fn c_opens_launch_palette_and_picks_an_agent() {
        let mut m = model_with_data();
        m.selected = 3; // Workspace(1) == repo_b @ /src/b
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
        let mut m = model_with_data(); // t1 is QUEUED
        m.selected = 1; // Agent(0) == t1
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
        m.selected = 1; // Agent(0) == t1
        update(&mut m, Key::Char('x'));
        assert_eq!(m.pending, Some(PendingAction::Forget("t1".into())));
        let effects = update(&mut m, Key::Esc);
        assert!(effects.is_empty());
        assert!(m.pending.is_none());
        assert_eq!(m.view, View::Home);
    }

    #[test]
    fn n_prefills_repo_from_selected_workspace() {
        let mut m = model_with_data();
        m.selected = 3; // Workspace(1) == repo_b @ /src/b
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
