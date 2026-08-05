//! `memmuxd` — the MemMux control-plane daemon and its control CLI.

use anyhow::Context;
use clap::{Parser, Subcommand};
use memmux_proto::{CreateTaskRequest, Request, Response, PROTOCOL_VERSION};
use memmux_sched::{ResourceEnvelope, GIB};
use memmux_store::Store;
use memmuxd::client::Client;
use memmuxd::daemon::{now_ms, DaemonState};
use memmuxd::server;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Parser, Debug)]
#[command(name = "memmuxd", version, about = "MemMux control-plane daemon")]
struct Cli {
    /// Managed root (defaults to $MEMMUX_ROOT or ~/.memmux).
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print daemon + protocol versions and exit.
    Version,
    /// Run the daemon (serves the UDS API until terminated).
    Serve,
    /// Query daemon info over the socket.
    Info,
    /// Create a task over the socket.
    Create {
        /// Task title / intent.
        #[arg(long)]
        title: String,
        /// Repository path.
        #[arg(long)]
        repo: String,
        /// Provider slug.
        #[arg(long, default_value = "claude-code")]
        provider: String,
        /// Base branch.
        #[arg(long, default_value = "main")]
        base: String,
        /// Command for the generic provider (everything after `--`).
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Admit and launch a task's provider over the socket.
    Start {
        /// Task id.
        id: String,
    },
    /// Freeze an idle task to a checkpoint and stop its provider.
    Hibernate {
        /// Task id.
        id: String,
    },
    /// Resume a hibernated task from its checkpoint.
    Resume {
        /// Task id.
        id: String,
    },
    /// Recycle a running provider (checkpoint, restart, resume).
    Recycle {
        /// Task id.
        id: String,
    },
    /// Manage workspaces (registered git repositories).
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCmd,
    },
    /// List tasks over the socket.
    List,
    /// Report memory pressure over the socket.
    Pressure,
    /// Export a diagnostics support bundle (reads the store directly).
    SupportBundle {
        /// Output directory.
        #[arg(long, default_value = "support-bundle")]
        out: PathBuf,
    },
    /// One-shot process-tree attribution snapshot against a pid.
    Snapshot {
        /// Root pid (defaults to this process).
        #[arg(long)]
        root_pid: Option<i32>,
    },
}

#[derive(Subcommand, Debug)]
enum WorkspaceCmd {
    /// Register a folder as a workspace (defaults to the current directory).
    Add {
        /// Path to the repository (or anywhere inside it).
        #[arg(default_value = ".")]
        path: String,
    },
    /// List registered workspaces.
    List,
    /// Remove a workspace by id.
    Rm {
        /// Workspace id.
        id: String,
    },
}

fn main() -> anyhow::Result<()> {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::INFO);
    tracing_subscriber::fmt().with_max_level(level).init();

    let cli = Cli::parse();
    let root = cli
        .root
        .or_else(|| std::env::var_os("MEMMUX_ROOT").map(PathBuf::from))
        .unwrap_or_else(default_root);
    let socket = root.join("memmux.sock");

    match cli.command {
        Command::Version => {
            println!(
                "memmuxd {} (protocol {})",
                env!("CARGO_PKG_VERSION"),
                PROTOCOL_VERSION
            );
        }
        Command::Serve => run_serve(&root, &socket)?,
        Command::Info => print_response(Client::new(&socket).call(&Request::DaemonInfo)?),
        Command::Create {
            title,
            repo,
            provider,
            base,
            cmd,
        } => {
            let req = Request::CreateTask(CreateTaskRequest {
                title,
                repository_path: repo,
                provider,
                base_branch: base,
                resource_class: None,
                priority: None,
                command: if cmd.is_empty() { None } else { Some(cmd) },
            });
            print_response(Client::new(&socket).call(&req)?);
        }
        Command::Start { id } => {
            print_response(Client::new(&socket).call(&Request::StartTask { id })?)
        }
        Command::Hibernate { id } => {
            print_response(Client::new(&socket).call(&Request::HibernateTask { id })?)
        }
        Command::Resume { id } => {
            print_response(Client::new(&socket).call(&Request::ResumeTask { id })?)
        }
        Command::Recycle { id } => {
            print_response(Client::new(&socket).call(&Request::RecycleTask { id })?)
        }
        Command::Workspace { action } => {
            let req = match action {
                WorkspaceCmd::Add { path } => Request::AddWorkspace { path },
                WorkspaceCmd::List => Request::ListWorkspaces,
                WorkspaceCmd::Rm { id } => Request::RemoveWorkspace { id },
            };
            print_response(Client::new(&socket).call(&req)?);
        }
        Command::List => print_response(Client::new(&socket).call(&Request::ListTasks)?),
        Command::Pressure => print_response(Client::new(&socket).call(&Request::SystemPressure)?),
        Command::SupportBundle { out } => export_support_bundle(&root, &out)?,
        Command::Snapshot { root_pid } => snapshot(root_pid)?,
    }
    Ok(())
}

fn run_serve(root: &std::path::Path, socket: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    let store = Store::open(root.join("state.db")).context("open durable store")?;
    let envelope = ResourceEnvelope::with_default_reserves(detect_physical_bytes());
    let state = Arc::new(Mutex::new(DaemonState::boot(
        store,
        envelope,
        root.to_path_buf(),
    )?));
    tracing::info!(
        tasks = state.lock().unwrap().task_count(),
        "recovered state on boot"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(server::serve(socket, state))
}

fn export_support_bundle(root: &std::path::Path, out: &std::path::Path) -> anyhow::Result<()> {
    let store = Store::open(root.join("state.db")).context("open store for support bundle")?;
    let envelope = ResourceEnvelope::with_default_reserves(detect_physical_bytes());
    let bundle = serde_json::json!({
        "generated_at_ms": now_ms(),
        "versions": { "daemon": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL_VERSION },
        "agent_budget_bytes": envelope.agent_budget_bytes,
        "physical_bytes": envelope.physical_bytes,
        "task_count": store.task_count()?,
        "recent_events": store.recent_events(200)?,
        "note": "Secrets are stored as references only; no secret material is included.",
    });
    std::fs::create_dir_all(out)?;
    let path = out.join(format!("memmux-support-{}.json", now_ms()));
    std::fs::write(&path, serde_json::to_string_pretty(&bundle)?)?;
    println!("wrote support bundle to {}", path.display());
    Ok(())
}

fn snapshot(root_pid: Option<i32>) -> anyhow::Result<()> {
    let sampler = memmux_metrics::default_sampler();
    let snapshot = sampler.snapshot()?;
    let root = root_pid.unwrap_or_else(|| std::process::id() as i32);
    let tree = memmux_metrics::ProcessTree::from_samples(snapshot.samples);
    println!(
        "root pid {root}: {} descendants, subtree RSS {:.1} MiB",
        tree.descendants(root).len(),
        tree.subtree_rss_bytes(root) as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}

fn print_response(resp: Response) {
    match resp {
        Response::DaemonInfo(i) => println!(
            "memmuxd {} (protocol {}), {} task(s), budget {} MiB",
            i.daemon_version,
            i.protocol_version,
            i.task_count,
            i.agent_budget_bytes / (1024 * 1024)
        ),
        Response::Task(t) => println!("{}  {:<12}  {}  [{}]", t.id, t.state, t.title, t.provider),
        Response::Tasks(ts) => {
            if ts.is_empty() {
                println!("(no tasks)");
            }
            for t in ts {
                println!("{}  {:<12}  {}  [{}]", t.id, t.state, t.title, t.provider);
            }
        }
        Response::Pressure(p) => println!(
            "pressure {} — {}% of {} MiB budget used",
            p.stage,
            p.utilization_pct,
            p.agent_budget_bytes / (1024 * 1024)
        ),
        Response::Events(evs) => {
            for e in evs {
                println!("#{} {} {} ({})", e.seq, e.event_type, e.source, e.category);
            }
        }
        Response::Screen(s) => {
            println!(
                "[{} rows, cursor {},{}, running={}]",
                s.rows.len(),
                s.cursor_row,
                s.cursor_col,
                s.running
            );
            for row in s.rows {
                println!("{row}");
            }
        }
        Response::History(h) => {
            println!(
                "[history {} lines, next_cursor {}, total {}]",
                h.lines.len(),
                h.next_cursor,
                h.total
            );
            for line in h.lines {
                println!("{line}");
            }
        }
        Response::Workspace(w) => {
            println!(
                "{}  {}  {}  ({} task(s))",
                w.id, w.name, w.path, w.task_count
            )
        }
        Response::Workspaces(ws) => {
            if ws.is_empty() {
                println!("(no workspaces)");
            }
            for w in ws {
                println!(
                    "{}  {}  {}  ({} task(s))",
                    w.id, w.name, w.path, w.task_count
                );
            }
        }
        Response::Ok => println!("ok"),
        Response::Error { message } => eprintln!("error: {message}"),
    }
}

fn default_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".memmux")
}

/// Best-effort physical memory detection, falling back to 16 GiB.
fn detect_physical_bytes() -> u64 {
    #[cfg(target_os = "macos")]
    {
        let mut size: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        let name = std::ffi::CString::new("hw.memsize").unwrap();
        // SAFETY: read-only sysctl; writes at most `len` bytes into `size`.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                &mut size as *mut u64 as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 && size > 0 {
            return size;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    if let Ok(kb) = rest.trim().trim_end_matches("kB").trim().parse::<u64>() {
                        return kb * 1024;
                    }
                }
            }
        }
    }
    16 * GIB
}
