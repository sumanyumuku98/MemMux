//! Launcher plugins (SUM-34 / SUM-35).
//!
//! A launcher starts **N identical stub agents** so they can be measured, and reports a
//! [`LaunchTopology`] that separates the *provider* process trees (the agents themselves) from
//! the *manager* process tree (the multiplexer's own server/daemon overhead). The benchmark
//! compares MemMux against a raw baseline, tmux, and — where installed — herdr.
//!
//! **Claims discipline (§19.5):** competitor launchers are only run if their binary is actually
//! present on `PATH`, and every step that drives an external tool propagates failures as an
//! [`io::Error`] so the run records the launcher as *skipped-with-reason* rather than fabricating
//! a competitor number. Absent tools report [`Launcher::is_available`] `== false` and are skipped.

use memmux_core::ids::Pid;
use memmux_metrics::{default_sampler, ProcessTree};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// The stub command run once per agent: `<bench_exe> stub --recording <recording_path>`.
///
/// Every launcher runs this identical command N times so the comparison is apples-to-apples.
fn stub_argv(spec: &LaunchSpec) -> Vec<String> {
    vec![
        spec.bench_exe.to_string_lossy().into_owned(),
        "stub".to_string(),
        "--recording".to_string(),
        spec.recording_path.to_string_lossy().into_owned(),
    ]
}

/// What category a launcher belongs to (used for reporting and fairness notes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherKind {
    /// Raw process / tmux baseline.
    Baseline,
    /// MemMux itself.
    MemMux,
    /// A third-party multiplexer invoked through its own CLI.
    External,
}

/// Where to find the stub recording and the binary that can execute it.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    /// Path to the JSON [`SessionRecording`](crate::stub::SessionRecording).
    pub recording_path: PathBuf,
    /// Path to the `memmux-bench` executable (invoked in `stub` mode).
    pub bench_exe: PathBuf,
}

/// The process topology of a launched session: which pids are agents vs. manager overhead.
///
/// A pid is a **provider** iff it lies in some [`agent_roots`](LaunchTopology::agent_roots)
/// subtree (an agent root plus its descendants). A pid is **manager overhead** iff it lies in
/// some [`manager_pids`](LaunchTopology::manager_pids) subtree but is not a provider. This lets
/// the sampler attribute a multiplexer's own server RSS separately from the agents it hosts.
#[derive(Clone, Debug, Default)]
pub struct LaunchTopology {
    /// Roots of the manager/server process trees (empty for the raw baseline).
    pub manager_pids: Vec<Pid>,
    /// Roots of the N agent process trees (one per launched stub).
    pub agent_roots: Vec<Pid>,
}

/// A pluggable way to launch N identical stub agents.
pub trait Launcher {
    /// Stable launcher name (appears in reports).
    fn name(&self) -> &str;
    /// Category of launcher.
    fn kind(&self) -> LauncherKind;
    /// Whether this launcher can run on the current host right now.
    fn is_available(&self) -> bool;
    /// Human-readable version of the launcher (e.g. `"tmux 3.6a"`, `"herdr 0.8.0"`,
    /// `"memmux 0.7.0"`); `"unknown"` when it cannot be resolved.
    fn version(&self) -> String;
    /// Start `n` identical stub agents described by `spec`, returning the running session.
    fn start(&self, n: usize, spec: &LaunchSpec) -> io::Result<Box<dyn LaunchedSession>>;
}

/// A running set of stub agents under one launcher, with its measured topology.
pub trait LaunchedSession {
    /// The provider/manager process topology of this session.
    fn topology(&self) -> &LaunchTopology;
    /// The pids the launcher's own machinery has surfaced as *escaped* (reparented out of every
    /// task subtree while still alive), deduped (SUM-166 / H3).
    ///
    /// Only MemMux has an escape-detection mechanism (its daemon emits `process_escaped` events),
    /// so only its `MemMuxSession` overrides this. The default is `None`, meaning "this launcher
    /// has no such capability" — which the report renders as *unsupported*, distinct from a
    /// measured zero. Called once, just before [`stop`](LaunchedSession::stop), while the daemon
    /// still lives.
    fn escaped_pids(&self) -> Option<Vec<Pid>> {
        None
    }
    /// Tear the whole session down. Best-effort: never panics, never leaves strays behind.
    fn stop(self: Box<Self>);
}

// ---------------------------------------------------------------------------------------------
// Raw baseline
// ---------------------------------------------------------------------------------------------

/// Baseline launcher: spawn N stub children directly, exactly as a raw terminal would.
#[derive(Debug, Default)]
pub struct RawLauncher;

/// A raw session: N stub child processes with no manager overhead.
#[derive(Debug)]
struct RawSession {
    topology: LaunchTopology,
    children: Vec<std::process::Child>,
}

impl Launcher for RawLauncher {
    fn name(&self) -> &str {
        "raw-baseline"
    }
    fn kind(&self) -> LauncherKind {
        LauncherKind::Baseline
    }
    fn is_available(&self) -> bool {
        true
    }
    fn version(&self) -> String {
        "raw (direct spawn)".to_string()
    }
    fn start(&self, n: usize, spec: &LaunchSpec) -> io::Result<Box<dyn LaunchedSession>> {
        let argv = stub_argv(spec);
        let mut children = Vec::with_capacity(n);
        let mut agent_roots = Vec::with_capacity(n);
        for _ in 0..n {
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let child = cmd.spawn()?;
            agent_roots.push(child.id() as Pid);
            children.push(child);
        }
        Ok(Box::new(RawSession {
            topology: LaunchTopology {
                manager_pids: Vec::new(),
                agent_roots,
            },
            children,
        }))
    }
}

impl LaunchedSession for RawSession {
    fn topology(&self) -> &LaunchTopology {
        &self.topology
    }
    fn stop(self: Box<Self>) {
        for mut child in self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// tmux (baseline, isolated server socket)
// ---------------------------------------------------------------------------------------------

/// tmux baseline launcher: drives an **isolated** tmux server (via a unique `-L <label>`
/// socket) so the user's own tmux is never touched. One pane per agent.
#[derive(Debug, Default)]
pub struct TmuxLauncher;

/// A running isolated tmux server hosting one stub per pane.
#[derive(Debug)]
struct TmuxSession {
    label: String,
    topology: LaunchTopology,
}

impl Launcher for TmuxLauncher {
    fn name(&self) -> &str {
        "tmux"
    }
    fn kind(&self) -> LauncherKind {
        LauncherKind::Baseline
    }
    fn is_available(&self) -> bool {
        binary_on_path("tmux").is_some()
    }
    fn version(&self) -> String {
        run_stdout("tmux", &["-V".to_string()])
            .ok()
            .and_then(|s| s.lines().next().map(str::trim).map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    }
    fn start(&self, n: usize, spec: &LaunchSpec) -> io::Result<Box<dyn LaunchedSession>> {
        if !self.is_available() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "tmux binary not found on PATH",
            ));
        }
        let label = format!("memmux-bench-{}", std::process::id());
        // The pane command is a single shell string; quote each argv element for `sh -c` safety.
        let cmd = shell_join(&stub_argv(spec));

        // First pane in a detached session with a fixed geometry (no attached client).
        tmux(
            &label,
            &["new-session", "-d", "-x", "200", "-y", "50", &cmd],
        )?;
        // Remaining panes as detached windows.
        for _ in 1..n {
            tmux(&label, &["new-window", "-d", &cmd])?;
        }

        // Give the shells a moment to fork the stub commands so pane pids are the stubs' shells.
        std::thread::sleep(Duration::from_millis(150));

        let panes = tmux(&label, &["list-panes", "-a", "-F", "#{pane_pid}"])?;
        let agent_roots: Vec<Pid> = panes
            .lines()
            .filter_map(|l| l.trim().parse::<Pid>().ok())
            .collect();
        if agent_roots.is_empty() {
            let _ = tmux(&label, &["kill-server"]);
            return Err(io::Error::other("tmux reported no panes after launch"));
        }
        let server_pid = tmux(&label, &["display-message", "-p", "#{pid}"])?
            .trim()
            .parse::<Pid>()
            .map_err(|e| io::Error::other(format!("could not parse tmux server pid: {e}")))?;

        Ok(Box::new(TmuxSession {
            label,
            topology: LaunchTopology {
                manager_pids: vec![server_pid],
                agent_roots,
            },
        }))
    }
}

impl LaunchedSession for TmuxSession {
    fn topology(&self) -> &LaunchTopology {
        &self.topology
    }
    fn stop(self: Box<Self>) {
        let _ = tmux(&self.label, &["kill-server"]);
    }
}

/// Run `tmux -L <label> <args...>`, returning stdout on success or an error on non-zero exit.
fn tmux(label: &str, args: &[&str]) -> io::Result<String> {
    let mut full: Vec<String> = vec!["-L".to_string(), label.to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    run_stdout("tmux", &full)
}

// ---------------------------------------------------------------------------------------------
// herdr (external, isolated headless session)
// ---------------------------------------------------------------------------------------------

/// herdr external launcher: drives an **isolated** headless herdr server bound to a unique
/// `--session` so the user's real herdr session is untouched. One pane per agent.
#[derive(Debug, Default)]
pub struct HerdrLauncher;

/// A running isolated herdr server hosting one stub per pane.
#[derive(Debug)]
struct HerdrSession {
    session: String,
    server: std::process::Child,
    topology: LaunchTopology,
}

impl Launcher for HerdrLauncher {
    fn name(&self) -> &str {
        "herdr"
    }
    fn kind(&self) -> LauncherKind {
        LauncherKind::External
    }
    fn is_available(&self) -> bool {
        binary_on_path("herdr").is_some()
    }
    fn version(&self) -> String {
        run_stdout("herdr", &["--version".to_string()])
            .ok()
            .and_then(|s| s.lines().next().map(str::trim).map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    }
    fn start(&self, n: usize, spec: &LaunchSpec) -> io::Result<Box<dyn LaunchedSession>> {
        if !self.is_available() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "herdr binary not found on PATH",
            ));
        }
        let session = format!("memmux-bench-{}", std::process::id());

        // Start an isolated headless server for this session (it does NOT auto-start on the
        // first client command). Detach stdio so it runs in the background.
        let server = Command::new("herdr")
            .args(["--session", &session, "server"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        // Guard so any error below tears the server down before returning.
        let result = (|| -> io::Result<LaunchTopology> {
            wait_for_herdr_ready(&session)?;

            // An active workspace is required before tabs/panes can be created; its root pane is
            // the first agent pane.
            herdr(
                &session,
                &["workspace", "create", "--cwd", "/tmp", "--no-focus"],
            )?;
            // One extra tab (pane) per remaining agent.
            for _ in 1..n {
                herdr(&session, &["tab", "create", "--cwd", "/tmp", "--no-focus"])?;
            }

            let pane_ids = herdr_pane_ids(&session)?;
            if pane_ids.len() < n {
                return Err(io::Error::other(format!(
                    "herdr created {} panes, expected {n}",
                    pane_ids.len()
                )));
            }

            // Run the identical stub command in each pane.
            let line = format!("{}\n", shell_join(&stub_argv(spec)));
            for pid in &pane_ids {
                herdr(&session, &["pane", "send-text", pid, &line])?;
            }

            // Let the shells fork the stub commands before we read pids.
            std::thread::sleep(Duration::from_millis(300));

            // Per-pane root pid is the pane's stable shell pid (the stub runs as its child).
            let mut agent_roots = Vec::with_capacity(pane_ids.len());
            for pid in &pane_ids {
                agent_roots.push(herdr_shell_pid(&session, pid)?);
            }

            // The manager pid is the herdr SERVER: the nearest "herdr"-named ancestor of a pane
            // shell pid (walking the live process tree). This isolates the server RSS.
            let manager_pid = herdr_server_pid(agent_roots[0]).ok_or_else(|| {
                io::Error::other("could not resolve the herdr server pid from a pane's ancestry")
            })?;

            Ok(LaunchTopology {
                manager_pids: vec![manager_pid],
                agent_roots,
            })
        })();

        match result {
            Ok(topology) => Ok(Box::new(HerdrSession {
                session,
                server,
                topology,
            })),
            Err(e) => {
                herdr_teardown(&session);
                let mut server = server;
                let _ = server.kill();
                let _ = server.wait();
                Err(e)
            }
        }
    }
}

impl LaunchedSession for HerdrSession {
    fn topology(&self) -> &LaunchTopology {
        &self.topology
    }
    fn stop(self: Box<Self>) {
        herdr_teardown(&self.session);
        let mut server = self.server;
        let _ = server.kill();
        let _ = server.wait();
    }
}

/// Stop AND delete an isolated benchmark session so herdr does not accumulate stopped sessions on
/// the host across repeated runs (delete only succeeds once the session is stopped).
fn herdr_teardown(session: &str) {
    let _ = run_stdout(
        "herdr",
        &["session".into(), "stop".into(), session.to_string()],
    );
    let _ = run_stdout(
        "herdr",
        &["session".into(), "delete".into(), session.to_string()],
    );
}

/// Run `herdr --session <session> <args...>`.
fn herdr(session: &str, args: &[&str]) -> io::Result<String> {
    let mut full: Vec<String> = vec!["--session".to_string(), session.to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    run_stdout("herdr", &full)
}

/// Poll `herdr pane list` until the isolated server answers (it starts asynchronously).
fn wait_for_herdr_ready(session: &str) -> io::Result<()> {
    for _ in 0..100 {
        if let Ok(out) = herdr(session, &["pane", "list"]) {
            if out.contains("\"pane_list\"") || out.contains("\"panes\"") {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "herdr headless server never became ready",
    ))
}

/// Parse the `pane_id` strings out of `herdr pane list` JSON (dependency-free extraction).
fn herdr_pane_ids(session: &str) -> io::Result<Vec<String>> {
    let out = herdr(session, &["pane", "list"])?;
    let ids = extract_json_strings(&out, "\"pane_id\":");
    if ids.is_empty() {
        return Err(io::Error::other("herdr pane list returned no pane ids"));
    }
    Ok(ids)
}

/// Read a pane's stable `shell_pid` from `herdr pane process-info`.
fn herdr_shell_pid(session: &str, pane_id: &str) -> io::Result<Pid> {
    let out = herdr(session, &["pane", "process-info", "--pane", pane_id])?;
    extract_json_int(&out, "\"shell_pid\":")
        .map(|v| v as Pid)
        .ok_or_else(|| {
            io::Error::other(format!("herdr process-info for {pane_id} had no shell_pid"))
        })
}

/// Walk up the live process tree from `pane_pid` to the nearest ancestor whose process name
/// contains `"herdr"` — that is the isolated headless server we spawned.
fn herdr_server_pid(pane_pid: Pid) -> Option<Pid> {
    let snapshot = default_sampler().snapshot().ok()?;
    let tree = ProcessTree::from_samples(snapshot.samples);
    for anc in tree.ancestors(pane_pid) {
        if let Some(sample) = tree.get(anc) {
            if sample.name.to_ascii_lowercase().contains("herdr") {
                return Some(anc);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// MemMux (the daemon under test)
// ---------------------------------------------------------------------------------------------

/// MemMux launcher: spawns the sibling `memmuxd` **binary** as an isolated subprocess (unique
/// socket + store dir) so its RSS is measured as manager overhead, then drives it over the UDS
/// client to create and start N generic stub tasks.
#[derive(Debug, Default)]
pub struct MemMuxLauncher;

/// A running isolated `memmuxd` daemon hosting N stub tasks.
struct MemMuxSession {
    daemon: std::process::Child,
    client: memmuxd::client::Client,
    task_ids: Vec<String>,
    root: PathBuf,
    topology: LaunchTopology,
}

impl std::fmt::Debug for MemMuxSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemMuxSession")
            .field("task_ids", &self.task_ids)
            .field("root", &self.root)
            .field("topology", &self.topology)
            .finish_non_exhaustive()
    }
}

impl Launcher for MemMuxLauncher {
    fn name(&self) -> &str {
        "memmux"
    }
    fn kind(&self) -> LauncherKind {
        LauncherKind::MemMux
    }
    fn is_available(&self) -> bool {
        memmuxd_binary().is_some()
    }
    fn version(&self) -> String {
        // The bench and the daemon share the workspace version.
        format!("memmux {}", env!("CARGO_PKG_VERSION"))
    }
    fn start(&self, n: usize, spec: &LaunchSpec) -> io::Result<Box<dyn LaunchedSession>> {
        let daemon_bin = memmuxd_binary().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not locate the sibling `memmuxd` binary next to the bench executable",
            )
        })?;

        // Isolated managed root (unique socket + store live under it). Keep the path SHORT: a
        // Unix-domain socket address must fit in `sockaddr_un` (~104 bytes on macOS), and the
        // default temp dir on macOS (`/var/folders/...`) is already long, so root directly under
        // the shortest available temp base with a compact unique name.
        let root = short_temp_base().join(format!("mmxb{}-{}", std::process::id(), short_uniq()));
        std::fs::create_dir_all(&root)?;
        let socket = root.join("memmux.sock");

        // The real server binary runs its own tokio pump loop, so nothing else is needed here.
        let daemon = Command::new(&daemon_bin)
            .arg("--root")
            .arg(&root)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let daemon_pid = daemon.id() as Pid;

        let result = (|| -> io::Result<(memmuxd::client::Client, Vec<String>, LaunchTopology)> {
            // Wait for the socket to appear.
            let client = memmuxd::client::Client::new(&socket);
            wait_for_socket(&socket)?;

            let argv = stub_argv(spec);
            // A neutral repo path (a temp dir); the generic provider falls back to it when it is
            // not a git repo, so no worktree is cut.
            let repo = root.join("repo");
            std::fs::create_dir_all(&repo)?;

            let mut task_ids = Vec::with_capacity(n);
            for i in 0..n {
                let req = memmux_proto::Request::CreateTask(memmux_proto::CreateTaskRequest {
                    title: format!("bench-{i}"),
                    repository_path: repo.to_string_lossy().into_owned(),
                    provider: "generic".to_string(),
                    base_branch: "main".to_string(),
                    resource_class: None,
                    priority: None,
                    command: Some(argv.clone()),
                });
                let id = match client.call(&req).map_err(anyhow_to_io)? {
                    memmux_proto::Response::Task(t) => t.id,
                    memmux_proto::Response::Error { message } => {
                        return Err(io::Error::other(format!(
                            "memmuxd rejected CreateTask: {message}"
                        )));
                    }
                    other => {
                        return Err(io::Error::other(format!(
                            "unexpected CreateTask response: {other:?}"
                        )));
                    }
                };
                match client
                    .call(&memmux_proto::Request::StartTask { id: id.clone() })
                    .map_err(anyhow_to_io)?
                {
                    memmux_proto::Response::Task(_) => {}
                    memmux_proto::Response::Error { message } => {
                        return Err(io::Error::other(format!(
                            "memmuxd rejected StartTask: {message}"
                        )));
                    }
                    other => {
                        return Err(io::Error::other(format!(
                            "unexpected StartTask response: {other:?}"
                        )));
                    }
                }
                task_ids.push(id);
            }

            // Poll until the tasks report ACTIVE (providers launched) — a few short tries.
            wait_until_active(&client, &task_ids);

            // The provider processes are the memmuxd daemon's direct children in the live tree.
            let agent_roots = daemon_provider_children(daemon_pid);
            if agent_roots.is_empty() {
                return Err(io::Error::other("memmuxd launched no provider processes"));
            }

            Ok((
                client,
                task_ids,
                LaunchTopology {
                    manager_pids: vec![daemon_pid],
                    agent_roots,
                },
            ))
        })();

        match result {
            Ok((client, task_ids, topology)) => Ok(Box::new(MemMuxSession {
                daemon,
                client,
                task_ids,
                root,
                topology,
            })),
            Err(e) => {
                let mut daemon = daemon;
                let _ = daemon.kill();
                let _ = daemon.wait();
                let _ = std::fs::remove_dir_all(&root);
                Err(e)
            }
        }
    }
}

impl MemMuxSession {
    /// Read the daemon's `process_escaped` events and return the deduped set of escaped pids
    /// (SUM-166 / H3).
    ///
    /// Pages every event (`after_seq: 0`) and parses the `pid` out of each `process_escaped`
    /// event's `payload_json`. Never fabricates: a client/transport error, or a daemon with no
    /// such events, yields an empty vec. This is the only launcher with an escape-detection
    /// mechanism, so it is the only one that returns `Some(..)` from
    /// [`escaped_pids`](LaunchedSession::escaped_pids).
    pub fn read_escaped_pids(&self) -> Vec<Pid> {
        let resp = match self.client.call(&memmux_proto::Request::ReadEvents {
            after_seq: 0,
            // A run injects one escape per agent and the daemon dedupes per pid, so a few thousand
            // events is far more than enough headroom for a benchmark run.
            limit: 10_000,
        }) {
            Ok(memmux_proto::Response::Events(events)) => events,
            _ => return Vec::new(),
        };
        let mut pids: Vec<Pid> = Vec::new();
        for ev in &resp {
            if ev.event_type != "process_escaped" {
                continue;
            }
            if let Some(pid) = ev
                .payload_json
                .as_deref()
                .and_then(escaped_pid_from_payload)
            {
                if !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
        }
        pids
    }
}

/// Parse the `pid` field out of a `process_escaped` event payload (a JSON object like
/// `{"pid":123,"name":"…","bytes":…}`). Dependency-free integer extraction, mirroring the herdr
/// helpers above.
fn escaped_pid_from_payload(payload: &str) -> Option<Pid> {
    extract_json_int(payload, "\"pid\":").map(|v| v as Pid)
}

impl LaunchedSession for MemMuxSession {
    fn topology(&self) -> &LaunchTopology {
        &self.topology
    }
    fn escaped_pids(&self) -> Option<Vec<Pid>> {
        Some(self.read_escaped_pids())
    }
    fn stop(self: Box<Self>) {
        // Best-effort: terminate each task, then kill+reap the daemon and clean the root.
        for id in &self.task_ids {
            let _ = self
                .client
                .call(&memmux_proto::Request::TerminateTask { id: id.clone() });
        }
        let mut daemon = self.daemon;
        let _ = daemon.kill();
        let _ = daemon.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The memmuxd daemon's direct child pids in the live tree — the PTY provider processes.
fn daemon_provider_children(daemon_pid: Pid) -> Vec<Pid> {
    match default_sampler().snapshot() {
        Ok(snapshot) => {
            let tree = ProcessTree::from_samples(snapshot.samples);
            tree.children(daemon_pid).to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// Poll `ListTasks` a few times, giving providers a moment to reach ACTIVE.
fn wait_until_active(client: &memmuxd::client::Client, ids: &[String]) {
    for _ in 0..40 {
        if let Ok(memmux_proto::Response::Tasks(tasks)) =
            client.call(&memmux_proto::Request::ListTasks)
        {
            let active = tasks
                .iter()
                .filter(|t| ids.contains(&t.id) && t.state == "ACTIVE")
                .count();
            if active >= ids.len() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Locate the sibling `memmuxd` binary: next to `current_exe()` (i.e. `target/<profile>/`), and
/// also next to the bench executable's parent, matching the `.exe` suffix on Windows.
fn memmuxd_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "memmuxd.exe"
    } else {
        "memmuxd"
    };
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // Same directory as the bench exe (target/<profile>/).
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    // `deps/` sibling (integration tests run from target/<profile>/deps/).
    if let Some(parent) = dir.parent() {
        let candidate = parent.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Finally, fall back to PATH.
    binary_on_path(name)
}

/// Wait up to a few seconds for the daemon socket file to appear.
fn wait_for_socket(socket: &Path) -> io::Result<()> {
    for _ in 0..300 {
        if socket.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "memmuxd socket never appeared",
    ))
}

/// Convert an `anyhow::Error` from the client into an `io::Error` for the launcher API.
fn anyhow_to_io(e: anyhow::Error) -> io::Error {
    io::Error::other(e.to_string())
}

// ---------------------------------------------------------------------------------------------
// External (dmux / cmux / agentmux) — retained but unavailable here
// ---------------------------------------------------------------------------------------------

/// A third-party multiplexer we do not yet drive correctly (dmux, cmux, agentmux). These are
/// retained in [`competitor_launchers`] for coverage tracking but are `is_available() == false`
/// on this host, so they are skipped rather than run with a guessed CLI (§19.5).
#[derive(Debug, Clone)]
pub struct ExternalLauncher {
    name: String,
    binary: String,
}

impl ExternalLauncher {
    /// Create an external launcher wrapping `binary`.
    pub fn new(name: impl Into<String>, binary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            binary: binary.into(),
        }
    }
}

impl Launcher for ExternalLauncher {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> LauncherKind {
        LauncherKind::External
    }
    fn is_available(&self) -> bool {
        // These tools have no validated headless CLI template yet (dmux needs a TTY; cmux and
        // agentmux are absent). We never fabricate their numbers, so they are always skipped.
        false
    }
    fn version(&self) -> String {
        "unknown".to_string()
    }
    fn start(&self, _n: usize, _spec: &LaunchSpec) -> io::Result<Box<dyn LaunchedSession>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "no validated headless launch template for '{}' (binary '{}')",
                self.name, self.binary
            ),
        ))
    }
}

/// The launchers the benchmark drives by default: the raw baseline, tmux, herdr, and MemMux.
///
/// tmux, herdr, and MemMux self-skip (`is_available() == false`) when their binary is absent, so
/// this set is safe to run on a bare CI host — only the raw baseline is guaranteed to run.
pub fn builtin_launchers() -> Vec<Box<dyn Launcher>> {
    vec![
        Box::new(RawLauncher),
        Box::new(TmuxLauncher),
        Box::new(HerdrLauncher),
        Box::new(MemMuxLauncher),
    ]
}

/// Competitor launcher plugins we do not yet drive headlessly (SUM-35).
///
/// dmux requires a TTY; cmux and agentmux are absent on the reference host. They are surfaced for
/// coverage tracking only and always report `is_available() == false` (§19.5 claims discipline).
pub fn competitor_launchers() -> Vec<Box<dyn Launcher>> {
    vec![
        Box::new(ExternalLauncher::new("dmux", "dmux")),
        Box::new(ExternalLauncher::new("cmux", "cmux")),
        Box::new(ExternalLauncher::new("agentmux", "agentmux")),
    ]
}

// ---------------------------------------------------------------------------------------------
// Small dependency-free helpers
// ---------------------------------------------------------------------------------------------

/// Run `bin args...`, capturing stdout; a non-zero exit is an [`io::Error`] carrying stderr.
fn run_stdout(bin: &str, args: &[String]) -> io::Result<String> {
    let output = Command::new(bin).args(args).stdin(Stdio::null()).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "`{bin}` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Join argv into a single POSIX-shell command string with each element single-quoted.
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract every string value following a JSON `key` (e.g. `"pane_id":`) from `text`.
///
/// A tiny hand-rolled scan (no JSON dependency): finds each occurrence of `key`, skips whitespace,
/// and reads the following `"..."` literal.
fn extract_json_strings(text: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find(key) {
        let mut i = from + rel + key.len();
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            out.push(text[start..i].to_string());
        }
        from += rel + key.len();
    }
    out
}

/// Extract the first integer value following a JSON `key` (e.g. `"shell_pid":`) from `text`.
fn extract_json_int(text: &str, key: &str) -> Option<i64> {
    let rel = text.find(key)?;
    let bytes = text.as_bytes();
    let mut i = rel + key.len();
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let start = i;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    text[start..i].parse::<i64>().ok()
}

/// A short base directory for the daemon's socket: prefer `/tmp` on unix (much shorter than the
/// per-user `/var/folders/...` `TMPDIR` on macOS, which can overflow `sockaddr_un`), else the
/// platform temp dir.
fn short_temp_base() -> PathBuf {
    #[cfg(unix)]
    {
        let tmp = Path::new("/tmp");
        if tmp.is_dir() {
            return tmp.to_path_buf();
        }
    }
    std::env::temp_dir()
}

/// A compact base-36 unique suffix (low bits of the epoch nanos) for temp paths.
fn short_uniq() -> String {
    let mut n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64;
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_else(|_| "0".to_string())
}

/// Look up an executable on `PATH` (a tiny, dependency-free `which`).
pub fn binary_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_and_memmux_kinds() {
        assert!(RawLauncher.is_available());
        assert_eq!(RawLauncher.kind(), LauncherKind::Baseline);
        assert_eq!(TmuxLauncher.kind(), LauncherKind::Baseline);
        assert_eq!(HerdrLauncher.kind(), LauncherKind::External);
        assert_eq!(MemMuxLauncher.kind(), LauncherKind::MemMux);
    }

    #[test]
    fn builtin_set_is_raw_tmux_herdr_memmux() {
        let names: Vec<String> = builtin_launchers()
            .iter()
            .map(|l| l.name().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "raw-baseline".to_string(),
                "tmux".to_string(),
                "herdr".to_string(),
                "memmux".to_string(),
            ]
        );
    }

    #[test]
    fn competitor_set_is_always_unavailable() {
        for l in competitor_launchers() {
            assert!(!l.is_available(), "{} should self-skip", l.name());
        }
    }

    #[test]
    fn raw_version_is_stable() {
        assert!(RawLauncher.version().contains("raw"));
        assert!(MemMuxLauncher.version().starts_with("memmux "));
    }

    #[test]
    fn common_binary_is_found_on_path() {
        #[cfg(unix)]
        assert!(binary_on_path("sh").is_some());
        assert!(binary_on_path("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn shell_join_quotes_each_arg() {
        let j = shell_join(&["a b".into(), "c".into()]);
        assert_eq!(j, "'a b' 'c'");
    }

    #[test]
    fn extract_json_helpers_parse_herdr_shapes() {
        let list = r#"{"result":{"panes":[{"pane_id":"w1:p1"},{"pane_id":"w1:p2"}]}}"#;
        assert_eq!(
            extract_json_strings(list, "\"pane_id\":"),
            vec!["w1:p1", "w1:p2"]
        );
        let info = r#"{"process_info":{"shell_pid": 45149,"pane_id":"w1:p1"}}"#;
        assert_eq!(extract_json_int(info, "\"shell_pid\":"), Some(45149));
        assert_eq!(extract_json_int(info, "\"missing\":"), None);
    }
}
