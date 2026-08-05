//! Launcher plugins (SUM-34 / SUM-35).
//!
//! A launcher is a way to *start* a stub session so it can be measured. The benchmark compares
//! MemMux against a raw baseline and, where they are installed, third-party multiplexers.
//!
//! **Claims discipline (§19.5):** competitor launchers are only run if their binary is actually
//! present on `PATH`. When absent they report [`Launcher::is_available`] `== false` and are
//! skipped — the harness never fabricates competitor numbers, matching the spec's
//! "no public control found" stance.

use memmux_core::ids::Pid;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// A spawned stub process under test.
#[derive(Debug)]
pub struct LaunchedProcess {
    /// Name of the launcher that started it.
    pub launcher: String,
    /// Root pid of the launched process.
    pub pid: Pid,
    /// The child handle.
    pub child: std::process::Child,
}

impl LaunchedProcess {
    /// Wait for the process to exit.
    pub fn wait(mut self) -> io::Result<std::process::ExitStatus> {
        self.child.wait()
    }
}

/// A pluggable way to launch a stub session.
pub trait Launcher {
    /// Stable launcher name (appears in reports).
    fn name(&self) -> &str;
    /// Category of launcher.
    fn kind(&self) -> LauncherKind;
    /// Whether this launcher can run on the current host right now.
    fn is_available(&self) -> bool;
    /// Launch the stub described by `spec`, returning the running process.
    fn launch(&self, spec: &LaunchSpec) -> io::Result<LaunchedProcess>;
}

/// Baseline launcher: spawn the stub directly, exactly as a raw terminal / tmux pane would.
#[derive(Debug, Default)]
pub struct RawLauncher;

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
    fn launch(&self, spec: &LaunchSpec) -> io::Result<LaunchedProcess> {
        spawn_stub(self.name(), spec, &[])
    }
}

/// MemMux launcher.
///
/// In Phase 0 there is no daemon yet, so this spawns the same stub as the baseline but tags it
/// as MemMux-managed (the `--managed` marker the process records for attribution). Real
/// admission/governance arrives with the daemon in Phase 1; until then this is deliberately an
/// honest apples-to-apples spawn.
#[derive(Debug, Default)]
pub struct MemMuxLauncher;

impl Launcher for MemMuxLauncher {
    fn name(&self) -> &str {
        "memmux"
    }
    fn kind(&self) -> LauncherKind {
        LauncherKind::MemMux
    }
    fn is_available(&self) -> bool {
        true
    }
    fn launch(&self, spec: &LaunchSpec) -> io::Result<LaunchedProcess> {
        spawn_stub(self.name(), spec, &["--managed".to_string()])
    }
}

/// A third-party multiplexer invoked through its own binary (dmux, cmux, Herdr, AgentMux).
#[derive(Debug, Clone)]
pub struct ExternalLauncher {
    name: String,
    binary: String,
    /// Argument template placed before the command to run; `{cmd}` is not expanded here — the
    /// external tool receives the stub invocation as trailing args.
    prefix_args: Vec<String>,
}

impl ExternalLauncher {
    /// Create an external launcher wrapping `binary`.
    pub fn new(
        name: impl Into<String>,
        binary: impl Into<String>,
        prefix_args: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            binary: binary.into(),
            prefix_args,
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
        binary_on_path(&self.binary).is_some()
    }
    fn launch(&self, spec: &LaunchSpec) -> io::Result<LaunchedProcess> {
        if !self.is_available() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} binary '{}' not found on PATH", self.name, self.binary),
            ));
        }
        // Invoke: <binary> <prefix_args...> <bench_exe> stub --recording <path>
        let mut cmd = Command::new(&self.binary);
        cmd.args(&self.prefix_args)
            .arg(&spec.bench_exe)
            .arg("stub")
            .arg("--recording")
            .arg(&spec.recording_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn()?;
        let pid = child.id() as Pid;
        Ok(LaunchedProcess {
            launcher: self.name.clone(),
            pid,
            child,
        })
    }
}

/// The launchers MemMux can drive correctly and compares by default: the raw baseline and
/// MemMux itself. Both run the identical stub, giving an honest apples-to-apples comparison.
pub fn builtin_launchers() -> Vec<Box<dyn Launcher>> {
    vec![Box::new(RawLauncher), Box::new(MemMuxLauncher)]
}

/// Competitor launcher plugins (SUM-35).
///
/// These are **opt-in** because each tool has its own CLI that MemMux cannot invoke correctly
/// without a validated command template; the `prefix_args` here are placeholders. Running a
/// competitor with a guessed CLI would produce misleading numbers, so the default benchmark
/// does not include them (§19.5 claims discipline). They are surfaced only via
/// `memmux-bench run --include-competitors` and still gated on the binary being present.
pub fn competitor_launchers() -> Vec<Box<dyn Launcher>> {
    vec![
        Box::new(ExternalLauncher::new("dmux", "dmux", vec!["run".into()])),
        Box::new(ExternalLauncher::new("cmux", "cmux", vec!["run".into()])),
        Box::new(ExternalLauncher::new("herdr", "herdr", vec!["run".into()])),
        Box::new(ExternalLauncher::new(
            "agentmux",
            "agentmux",
            vec!["run".into()],
        )),
    ]
}

/// Spawn the bench binary in `stub` mode; stdout/stderr are discarded (we measure memory, not
/// content). `extra` args are appended after the recording path.
fn spawn_stub(launcher: &str, spec: &LaunchSpec, extra: &[String]) -> io::Result<LaunchedProcess> {
    let mut cmd = Command::new(&spec.bench_exe);
    cmd.arg("stub")
        .arg("--recording")
        .arg(&spec.recording_path)
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = cmd.spawn()?;
    let pid = child.id() as Pid;
    Ok(LaunchedProcess {
        launcher: launcher.to_string(),
        pid,
        child,
    })
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
    fn baseline_and_memmux_are_always_available() {
        assert!(RawLauncher.is_available());
        assert!(MemMuxLauncher.is_available());
        assert_eq!(RawLauncher.kind(), LauncherKind::Baseline);
        assert_eq!(MemMuxLauncher.kind(), LauncherKind::MemMux);
    }

    #[test]
    fn builtin_set_is_baseline_and_memmux_only() {
        let names: Vec<String> = builtin_launchers()
            .iter()
            .map(|l| l.name().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["raw-baseline".to_string(), "memmux".to_string()]
        );
    }

    #[test]
    fn competitor_set_contains_the_four_plugins() {
        let names: Vec<String> = competitor_launchers()
            .iter()
            .map(|l| l.name().to_string())
            .collect();
        for expected in ["dmux", "cmux", "herdr", "agentmux"] {
            assert!(names.contains(&expected.to_string()));
        }
    }

    #[test]
    fn missing_external_binary_is_unavailable() {
        let l = ExternalLauncher::new("nope", "definitely-not-a-real-binary-xyz", vec![]);
        assert!(!l.is_available());
    }

    #[test]
    fn common_binary_is_found_on_path() {
        // `sh` exists on every unix CI host.
        #[cfg(unix)]
        assert!(binary_on_path("sh").is_some());
        assert!(binary_on_path("definitely-not-a-real-binary-xyz").is_none());
    }
}
