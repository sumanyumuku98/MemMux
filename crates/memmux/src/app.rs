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

/// The top-level views. One home layout + modals (SUM-127).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Sidebar of workspaces → agents + a main detail pane.
    Home,
    /// New-task form with provider picker (SUM-87).
    NewTask,
    /// Modal to open a folder as a workspace (SUM-124).
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
    /// Admit and launch a task's provider.
    StartTask(String),
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
    /// Text buffer for the open-folder modal (SUM-124).
    pub folder_input: String,
    /// The git root of the directory `memmux` was launched from, if any — the default repo for the
    /// new-task form (SUM-119). Set by the runtime; `update` only reads it.
    pub cwd_repo: Option<String>,
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
            folder_input: String::new(),
            cwd_repo: None,
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
        Key::Char('o') => {
            model.folder_input = String::new();
            model.view = View::OpenFolder;
        }
        Key::Char('n') => {
            model.form = NewTaskForm {
                repo: model.default_repo(),
                ..NewTaskForm::default()
            };
            model.view = View::NewTask;
        }
        Key::Up | Key::Char('k') => move_selection(model, -1),
        Key::Down | Key::Char('j') => move_selection(model, 1),
        Key::Enter => return activate_selection(model),
        _ => {}
    }
    Vec::new()
}

/// Act on the selected sidebar row: attach to an agent, or launch a new agent into a workspace.
fn activate_selection(model: &mut Model) -> Vec<Effect> {
    match model.selected_nav() {
        Some(NavItem::Agent(ti)) => {
            if let Some(id) = model.data.tasks.get(ti).map(|t| t.id.clone()) {
                // Interactive on open (SUM-125): start + attach.
                return vec![Effect::StartTask(id.clone()), Effect::Attach(id)];
            }
        }
        Some(NavItem::Workspace(_)) | Some(NavItem::Unregistered) | None => {
            // Launch a new agent into the selected workspace (repo prefilled).
            model.form = NewTaskForm {
                repo: model.default_repo(),
                ..NewTaskForm::default()
            };
            model.view = View::NewTask;
        }
    }
    Vec::new()
}

fn move_selection(model: &mut Model, delta: isize) {
    let len = model.nav_items().len();
    if len == 0 {
        return;
    }
    let next = (model.selected as isize + delta).clamp(0, len as isize - 1);
    model.selected = next as usize;
}

fn update_open_folder(model: &mut Model, key: Key) -> Vec<Effect> {
    match key {
        Key::Esc => model.view = View::Home,
        Key::Backspace => {
            model.folder_input.pop();
        }
        Key::Char(c) => model.folder_input.push(c),
        Key::Enter => {
            let path = model.folder_input.trim().to_string();
            if path.is_empty() {
                model.status = "enter a folder path".to_string();
                return Vec::new();
            }
            model.status = format!("opening {path}");
            model.view = View::Home;
            return vec![Effect::AddWorkspace(path), Effect::Refresh];
        }
        _ => {}
    }
    Vec::new()
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
    fn enter_on_agent_starts_and_attaches() {
        let mut m = model_with_data();
        m.selected = 1; // Agent(0) == t1
        let effects = update(&mut m, Key::Enter);
        assert!(effects.contains(&Effect::StartTask("t1".into())));
        assert!(effects.contains(&Effect::Attach("t1".into())));
    }

    #[test]
    fn enter_on_workspace_opens_new_task_prefilled() {
        let mut m = model_with_data();
        m.selected = 0; // Workspace(0) == repo_a @ /src/a
        update(&mut m, Key::Enter);
        assert_eq!(m.view, View::NewTask);
        assert_eq!(m.form.repo, "/src/a");
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
    fn open_folder_flow_adds_a_workspace() {
        let mut m = Model::default();
        update(&mut m, Key::Char('o'));
        assert_eq!(m.view, View::OpenFolder);
        for c in "/src/app".chars() {
            update(&mut m, Key::Char(c));
        }
        let effects = update(&mut m, Key::Enter);
        assert!(effects.contains(&Effect::AddWorkspace("/src/app".to_string())));
        assert_eq!(m.view, View::Home);
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
