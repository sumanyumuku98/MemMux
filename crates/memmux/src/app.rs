//! The Elm-architecture core of the TUI (SUM-81): a `Model`, an abstract `Key`, and a pure
//! `update` that returns the next model plus [`Effect`]s for the runtime to execute. Keeping
//! `update` free of terminal and socket I/O makes the whole interaction layer unit-testable.

use memmux_proto::{
    CreateTaskRequest, DaemonInfo, EventView, PressureView, TaskView, WorkspaceView,
};

/// Provider slugs offered by the new-task form's picker (SUM-87).
pub const PROVIDERS: [&str; 5] = ["claude-code", "codex", "gemini-cli", "opencode", "generic"];

/// The top-level views (§ Appendix A dashboard + the operational panes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// System + budget + tasks + top descendants (Appendix A).
    Dashboard,
    /// Task table with navigation (SUM-83).
    Tasks,
    /// Queue view: admission scores + waits (SUM-88).
    Queue,
    /// Resource / event timeline (SUM-89).
    Timeline,
    /// Registered workspaces, tasks grouped under them (SUM-124).
    Workspaces,
    /// New-task form with provider picker (SUM-87).
    NewTask,
    /// Modal to open a folder as a workspace (SUM-124).
    OpenFolder,
    /// Live terminal view of one task: screen grid + scrollback (SUM-84/85/86).
    Term,
    /// Help / keymap (SUM-89).
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

    /// Build a create request if the form is complete enough.
    pub fn to_request(&self) -> Option<CreateTaskRequest> {
        if self.title.trim().is_empty() || self.repo.trim().is_empty() {
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
    /// Tasks.
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

/// A side effect for the runtime to perform (I/O the pure `update` cannot do).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Re-fetch data from the daemon.
    Refresh,
    /// Create a task.
    CreateTask(CreateTaskRequest),
    /// Admit and launch a task's provider.
    StartTask(String),
    /// Load the current screen grid for a task.
    LoadScreen(String),
    /// Load a page of scrollback history for a task.
    LoadHistory {
        /// Task id.
        id: String,
        /// Starting line index.
        cursor: u64,
    },
    /// Enter interactive attach passthrough for a task (SUM-86; runtime handles the raw loop).
    Attach(String),
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
    /// Selected row index (task/queue lists).
    pub selected: usize,
    /// Scroll offset for the timeline.
    pub scroll: usize,
    /// New-task form.
    pub form: NewTaskForm,
    /// The task whose terminal is being viewed (Term view).
    pub focused_task: Option<String>,
    /// Current screen grid rows for the focused task.
    pub screen_rows: Vec<String>,
    /// Scrollback history lines for the focused task.
    pub history_rows: Vec<String>,
    /// Whether the Term view shows scrollback history rather than the live screen.
    pub show_history: bool,
    /// Text buffer for the open-folder modal (SUM-124).
    pub folder_input: String,
    /// Status line message.
    pub status: String,
    /// Whether to exit.
    pub should_quit: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            view: View::Dashboard,
            data: Data::default(),
            selected: 0,
            scroll: 0,
            form: NewTaskForm::default(),
            focused_task: None,
            screen_rows: Vec::new(),
            history_rows: Vec::new(),
            show_history: false,
            folder_input: String::new(),
            status: "connected".to_string(),
            should_quit: false,
        }
    }
}

impl Model {
    /// The currently selected task, if any.
    pub fn selected_task(&self) -> Option<&TaskView> {
        self.data.tasks.get(self.selected)
    }

    /// The currently selected workspace, if any (Workspaces view).
    pub fn selected_workspace(&self) -> Option<&WorkspaceView> {
        self.data.workspaces.get(self.selected)
    }

    /// Apply loaded data, clamping the selection against the current view's list.
    pub fn set_data(&mut self, data: Data) {
        self.data = data;
        let len = match self.view {
            View::Workspaces => self.data.workspaces.len(),
            _ => self.data.tasks.len(),
        };
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
    }
}

/// The pure state transition. Returns effects the runtime should execute.
pub fn update(model: &mut Model, key: Key) -> Vec<Effect> {
    // Modal views handle their own keys.
    if model.view == View::NewTask {
        return update_form(model, key);
    }
    if model.view == View::OpenFolder {
        return update_open_folder(model, key);
    }
    if model.view == View::Term {
        return update_term(model, key);
    }

    match key {
        Key::Quit => model.should_quit = true,
        Key::Char('q') => model.should_quit = true,
        Key::Char('1') => model.view = View::Dashboard,
        Key::Char('2') => model.view = View::Tasks,
        Key::Char('3') => model.view = View::Queue,
        Key::Char('4') => model.view = View::Timeline,
        Key::Char('5') => {
            model.view = View::Workspaces;
            model.selected = 0;
        }
        // Open a folder as a workspace (SUM-124).
        Key::Char('o') => {
            model.folder_input = String::new();
            model.view = View::OpenFolder;
        }
        Key::Char('n') => {
            // From the Workspaces view, prefill the repo with the selected workspace so launching
            // an agent into it needs no manual path ("add agent to this workspace").
            let mut form = NewTaskForm::default();
            if model.view == View::Workspaces {
                if let Some(ws) = model.selected_workspace() {
                    form.repo = ws.path.clone();
                }
            }
            model.form = form;
            model.view = View::NewTask;
        }
        Key::Char('?') | Key::Char('h') => model.view = View::Help,
        Key::Esc => model.view = View::Dashboard,
        Key::Up | Key::Char('k') => move_selection(model, -1),
        Key::Down | Key::Char('j') => move_selection(model, 1),
        Key::Enter => match model.view {
            // Enter opens (and starts) the selected task's terminal view.
            View::Dashboard | View::Tasks => {
                if let Some(id) = model.selected_task().map(|t| t.id.clone()) {
                    model.focused_task = Some(id.clone());
                    model.screen_rows.clear();
                    model.history_rows.clear();
                    model.show_history = false;
                    model.view = View::Term;
                    return vec![Effect::StartTask(id.clone()), Effect::LoadScreen(id)];
                }
            }
            // Enter on a workspace launches an agent into it (repo prefilled).
            View::Workspaces => {
                let mut form = NewTaskForm::default();
                if let Some(ws) = model.selected_workspace() {
                    form.repo = ws.path.clone();
                }
                model.form = form;
                model.view = View::NewTask;
            }
            _ => {}
        },
        _ => {}
    }
    Vec::new()
}

/// Keys for the open-folder modal (SUM-124).
fn update_open_folder(model: &mut Model, key: Key) -> Vec<Effect> {
    match key {
        Key::Esc => model.view = View::Workspaces,
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
            model.view = View::Workspaces;
            return vec![Effect::AddWorkspace(path), Effect::Refresh];
        }
        _ => {}
    }
    Vec::new()
}

/// Keys for the live terminal view (SUM-84/85/86).
fn update_term(model: &mut Model, key: Key) -> Vec<Effect> {
    let Some(id) = model.focused_task.clone() else {
        model.view = View::Tasks;
        return Vec::new();
    };
    match key {
        Key::Esc | Key::Char('q') => {
            model.view = View::Tasks;
            model.focused_task = None;
        }
        // Attach: hand off to the runtime's raw passthrough loop (SUM-86).
        Key::Char('a') => return vec![Effect::Attach(id)],
        // Toggle scrollback history (SUM-85).
        Key::Char('h') => {
            model.show_history = !model.show_history;
            if model.show_history {
                return vec![Effect::LoadHistory { id, cursor: 0 }];
            }
        }
        // Refresh the live screen.
        Key::Char('r') => return vec![Effect::LoadScreen(id)],
        _ => {}
    }
    Vec::new()
}

fn move_selection(model: &mut Model, delta: isize) {
    match model.view {
        View::Timeline => {
            let max = model.data.events.len().saturating_sub(1);
            model.scroll = (model.scroll as isize + delta).clamp(0, max as isize) as usize;
        }
        _ => {
            let len = match model.view {
                View::Workspaces => model.data.workspaces.len(),
                _ => model.data.tasks.len(),
            };
            if len == 0 {
                return;
            }
            let next = (model.selected as isize + delta).clamp(0, len as isize - 1);
            model.selected = next as usize;
        }
    }
}

fn update_form(model: &mut Model, key: Key) -> Vec<Effect> {
    match key {
        Key::Esc => {
            model.view = View::Dashboard;
        }
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
                model.status = format!("creating task '{}'", req.title);
                model.view = View::Tasks;
                return vec![Effect::CreateTask(req), Effect::Refresh];
            }
            model.status = "title and repo are required".to_string();
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

    fn task(id: &str) -> TaskView {
        TaskView {
            id: id.into(),
            title: format!("title {id}"),
            provider: "codex".into(),
            state: "QUEUED".into(),
            repository: "repo_1".into(),
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

    #[test]
    fn open_folder_flow_adds_a_workspace() {
        let mut m = Model::default();
        update(&mut m, Key::Char('o'));
        assert_eq!(m.view, View::OpenFolder);
        for c in "/src/app".chars() {
            update(&mut m, Key::Char(c));
        }
        assert_eq!(m.folder_input, "/src/app");
        let effects = update(&mut m, Key::Enter);
        assert!(effects.contains(&Effect::AddWorkspace("/src/app".to_string())));
        assert!(effects.contains(&Effect::Refresh));
        assert_eq!(m.view, View::Workspaces);
    }

    #[test]
    fn enter_on_workspace_prefills_the_new_task_repo() {
        let mut m = Model {
            view: View::Workspaces,
            ..Model::default()
        };
        m.set_data(Data {
            workspaces: vec![workspace("product", "/src/product")],
            ..Default::default()
        });
        update(&mut m, Key::Enter);
        assert_eq!(m.view, View::NewTask);
        assert_eq!(
            m.form.repo, "/src/product",
            "repo should default to the workspace path"
        );
    }

    #[test]
    fn number_keys_switch_views() {
        let mut m = Model::default();
        update(&mut m, Key::Char('2'));
        assert_eq!(m.view, View::Tasks);
        update(&mut m, Key::Char('3'));
        assert_eq!(m.view, View::Queue);
        update(&mut m, Key::Char('?'));
        assert_eq!(m.view, View::Help);
    }

    #[test]
    fn quit_sets_flag() {
        let mut m = Model::default();
        update(&mut m, Key::Char('q'));
        assert!(m.should_quit);
    }

    #[test]
    fn selection_moves_and_clamps() {
        let mut m = Model {
            view: View::Tasks,
            ..Model::default()
        };
        m.set_data(Data {
            tasks: vec![task("a"), task("b"), task("c")],
            ..Default::default()
        });
        update(&mut m, Key::Down);
        assert_eq!(m.selected, 1);
        update(&mut m, Key::Down);
        update(&mut m, Key::Down); // clamps at 2
        assert_eq!(m.selected, 2);
        update(&mut m, Key::Up);
        assert_eq!(m.selected, 1);
        assert_eq!(m.selected_task().unwrap().id, "b");
    }

    #[test]
    fn new_task_form_types_and_submits() {
        let mut m = Model::default();
        update(&mut m, Key::Char('n'));
        assert_eq!(m.view, View::NewTask);
        for c in "Refactor".chars() {
            update(&mut m, Key::Char(c));
        }
        assert_eq!(m.form.title, "Refactor");
        update(&mut m, Key::Tab); // -> Repo
        for c in "/src".chars() {
            update(&mut m, Key::Char(c));
        }
        update(&mut m, Key::Tab); // -> Provider
        update(&mut m, Key::Right); // codex
        assert_eq!(m.form.provider(), "codex");

        let effects = update(&mut m, Key::Enter);
        assert!(effects.contains(&Effect::Refresh));
        assert!(effects.iter().any(
            |e| matches!(e, Effect::CreateTask(r) if r.title == "Refactor" && r.provider == "codex")
        ));
        assert_eq!(m.view, View::Tasks);
    }

    #[test]
    fn form_requires_title_and_repo() {
        let mut m = Model {
            view: View::NewTask,
            ..Model::default()
        };
        let effects = update(&mut m, Key::Enter);
        assert!(effects.is_empty());
        assert!(m.status.contains("required"));
    }

    #[test]
    fn backspace_edits_focused_field() {
        let mut m = Model {
            view: View::NewTask,
            ..Model::default()
        };
        for c in "abc".chars() {
            update(&mut m, Key::Char(c));
        }
        update(&mut m, Key::Backspace);
        assert_eq!(m.form.title, "ab");
    }

    #[test]
    fn enter_opens_term_starts_task_and_streams() {
        let mut m = Model {
            view: View::Tasks,
            ..Model::default()
        };
        m.set_data(Data {
            tasks: vec![task("t1")],
            ..Default::default()
        });

        let effects = update(&mut m, Key::Enter);
        assert_eq!(m.view, View::Term);
        assert_eq!(m.focused_task.as_deref(), Some("t1"));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::StartTask(id) if id == "t1")));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::LoadScreen(id) if id == "t1")));

        // 'h' toggles scrollback and requests a history page (SUM-85).
        let e2 = update(&mut m, Key::Char('h'));
        assert!(m.show_history);
        assert!(e2
            .iter()
            .any(|e| matches!(e, Effect::LoadHistory { id, .. } if id == "t1")));

        // 'a' requests attach passthrough (SUM-86).
        let e3 = update(&mut m, Key::Char('a'));
        assert!(e3
            .iter()
            .any(|e| matches!(e, Effect::Attach(id) if id == "t1")));

        // Esc returns to the task list.
        update(&mut m, Key::Esc);
        assert_eq!(m.view, View::Tasks);
        assert!(m.focused_task.is_none());
    }
}
