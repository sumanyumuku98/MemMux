//! `memmux-bench` — CLI for the MemMux benchmark harness.

use clap::{Parser, Subcommand};
use memmux_bench::host::HostSpec;
use memmux_bench::launcher::{builtin_launchers, competitor_launchers, Launcher};
use memmux_bench::matrix::TestMatrix;
use memmux_bench::report::ReportMeta;
use memmux_bench::run::{run_benchmark, RunConfig};
use memmux_bench::scenario::Scenario;
use memmux_bench::stub::{
    run_child_worker, run_orphan_child, run_orphan_intermediate, SessionRecording,
};
use memmux_bench::sweep::parse_agents_sweep;
use memmux_core::Provider;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "memmux-bench",
    version,
    about = "MemMux competitive benchmark harness"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Execute a stub session recording (used internally by launchers).
    Stub {
        /// Path to a JSON session recording.
        #[arg(long)]
        recording: PathBuf,
        /// Marker set by the MemMux launcher (currently informational).
        #[arg(long, default_value_t = false)]
        managed: bool,
    },
    /// Child worker process spawned by a stub (used internally).
    StubChild {
        /// Mebibytes to hold resident.
        #[arg(long)]
        mib: u64,
        /// How long to stay alive, in milliseconds.
        #[arg(long)]
        hold_ms: u64,
    },
    /// Intermediate half of the escape double-fork spawned by a stub (used internally): forks a
    /// detached grandchild, stays alive `settle_ms`, then exits so the grandchild reparents to
    /// init (SUM-166 / H3).
    #[command(hide = true)]
    StubOrphan {
        /// How long the intermediate stays alive before exiting, in milliseconds.
        #[arg(long)]
        settle_ms: u64,
        /// How long the escaped grandchild holds memory after reparenting, in milliseconds.
        #[arg(long)]
        hold_ms: u64,
    },
    /// Escaped grandchild spawned by `stub-orphan` (used internally): hold a little memory then
    /// exit (SUM-166 / H3).
    #[command(hide = true)]
    StubOrphanChild {
        /// Mebibytes to hold resident.
        #[arg(long)]
        mib: u64,
        /// How long to stay alive, in milliseconds.
        #[arg(long)]
        hold_ms: u64,
    },
    /// Run the benchmark across all available launchers and emit a report.
    Run {
        /// Scenario to run (`all`, `burst`, `soak`, `idle`, `leak`, `hold`, `escape`, `overcommit`).
        #[arg(long, default_value = "all")]
        scenario: String,
        /// Provider profile to emulate.
        #[arg(long, default_value = "generic")]
        provider: String,
        /// Workload intensity (1 = fast smoke run).
        #[arg(long, default_value_t = 1)]
        intensity: u64,
        /// Milliseconds between samples.
        #[arg(long, default_value_t = 100)]
        interval_ms: u64,
        /// Maximum samples per run.
        #[arg(long, default_value_t = 20)]
        max_samples: usize,
        /// Number of identical stub agents to launch per launcher.
        #[arg(long, default_value_t = 3)]
        agents: usize,
        /// Agent-count sweep as a comma list (e.g. `1,5,10,20`); when present it OVERRIDES
        /// `--agents` and runs each (launcher × scenario) once per N (SUM-169 / P3).
        #[arg(long)]
        agents_sweep: Option<String>,
        /// Number of repeated trials per (launcher, scenario) for mean/CI statistics (SUM-162).
        #[arg(long, default_value_t = 1)]
        trials: usize,
        /// Constrained MemMux agent budget in MiB for the `overcommit` scenario (SUM-167 / H4).
        /// Set below the aggregate predicted peak of N agents to force overcommit; ignored by every
        /// other scenario and by non-MemMux launchers. `0`/unset leaves the host-derived default.
        #[arg(long)]
        agent_budget_mib: Option<u64>,
        /// Run a REAL external command as each agent (e.g. `claude -p "add a greet() function"`)
        /// instead of the built-in deterministic stub. Every launcher runs `sh -lc "<cmd>"`; the
        /// SAME footprint/attribution/teardown/cleanup harness then measures the real agent process
        /// tree. Forces hold-style semantics (launch + sample + teardown + measure cleanup); the
        /// stub-only escape/overcommit injection is skipped. Unset = stub as today.
        #[arg(long)]
        agent_cmd: Option<String>,
        /// Working directory the `--agent-cmd` command runs in. Ignored unless `--agent-cmd` is set;
        /// defaults to the current directory when omitted.
        #[arg(long)]
        agent_cwd: Option<PathBuf>,
        /// Also list competitor launchers (dmux/cmux/agentmux) — currently always skipped.
        #[arg(long, default_value_t = false)]
        include_competitors: bool,
        /// Output directory for JSONL + report.
        #[arg(long, default_value = "bench-out")]
        out: PathBuf,
    },
    /// One-command reproducer (SUM-171 / P3): run the canonical paper matrix and write a
    /// self-contained artifact dir (all JSONL + `report.md` with a host-spec header + figures +
    /// `host.json`). Every flag below is an OPTIONAL smoke override of the preset so it can be run
    /// tiny in CI/dev; with no overrides it runs the full paper matrix.
    Paper {
        /// Output artifact directory.
        #[arg(long, default_value = "paper-out")]
        out: PathBuf,
        /// Provider profile to emulate.
        #[arg(long, default_value = "generic")]
        provider: String,
        /// Override the canonical N-sweep (default `1,5,10,20`).
        #[arg(long)]
        agents_sweep: Option<String>,
        /// Override the canonical scenario set (comma list; default `hold,escape,overcommit`).
        #[arg(long)]
        scenarios: Option<String>,
        /// Override the trial count (default 3).
        #[arg(long)]
        trials: Option<usize>,
        /// Override the max samples per run (default 20).
        #[arg(long)]
        max_samples: Option<usize>,
        /// Override the sampling interval in ms (default 1000).
        #[arg(long)]
        interval_ms: Option<u64>,
        /// Constrained MemMux agent budget in MiB for the overcommit run (default 400).
        #[arg(long)]
        agent_budget_mib: Option<u64>,
    },
    /// List the benchmark scenarios.
    Scenarios,
    /// Report the §18.2 test matrix size and the subset runnable on this host.
    Matrix {
        /// Host memory in GiB (defaults to detected physical memory).
        #[arg(long)]
        host_mem_gib: Option<u32>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Stub {
            recording,
            managed: _,
        } => {
            let data = std::fs::read(&recording)?;
            let recording: SessionRecording = serde_json::from_slice(&data)?;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            recording.execute(&mut lock)?;
        }
        Command::StubChild { mib, hold_ms } => {
            run_child_worker(mib, hold_ms);
        }
        Command::StubOrphan { settle_ms, hold_ms } => {
            run_orphan_intermediate(settle_ms, hold_ms);
        }
        Command::StubOrphanChild { mib, hold_ms } => {
            run_orphan_child(mib, hold_ms);
        }
        Command::Run {
            scenario,
            provider,
            intensity,
            interval_ms,
            max_samples,
            agents,
            agents_sweep,
            trials,
            agent_budget_mib,
            agent_cmd,
            agent_cwd,
            include_competitors,
            out,
        } => {
            let provider = parse_provider(&provider)?;
            // A real agent command forces hold-style semantics: ignore any escape/overcommit
            // selection (their stub-only injection cannot apply to an arbitrary command) and run
            // the `hold` scenario — launch + sample footprint + teardown + measure cleanup.
            let scenarios = if agent_cmd.is_some() {
                let requested = parse_scenarios(&scenario)?;
                if requested
                    .iter()
                    .any(|s| matches!(s, Scenario::Escape | Scenario::Overcommit))
                {
                    eprintln!(
                        "note: --agent-cmd is set, so escape/overcommit selections are ignored; \
                         running the `hold` scenario (launch + sample + teardown + cleanup)."
                    );
                }
                vec![Scenario::Hold]
            } else {
                parse_scenarios(&scenario)?
            };
            let agent_budget_bytes = agent_budget_mib.filter(|&m| m > 0).map(|m| m * 1024 * 1024);
            let agents_sweep = match agents_sweep {
                Some(list) => Some(
                    parse_agents_sweep(&list)
                        .map_err(|e| anyhow::anyhow!("invalid --agents-sweep '{list}': {e}"))?,
                ),
                None => None,
            };
            let cfg = RunConfig {
                provider,
                intensity,
                interval_ms,
                max_samples,
                agents,
                agents_sweep,
                trials,
                bench_exe: std::env::current_exe()?,
                workdir: out.clone(),
                agent_budget_bytes,
                agent_cmd,
                agent_cwd,
            };
            let mut launchers: Vec<Box<dyn Launcher>> = builtin_launchers();
            if include_competitors {
                launchers.extend(competitor_launchers());
            }
            let available: Vec<&str> = launchers
                .iter()
                .filter(|l| l.is_available())
                .map(|l| l.name())
                .collect();
            eprintln!("available launchers: {}", available.join(", "));

            let outcome = run_benchmark(&launchers, &scenarios, &cfg)?;
            for (name, reason) in &outcome.skipped {
                eprintln!("skipped {name}: {reason}");
            }
            let report = outcome.to_markdown("MemMux Phase 0 benchmark");
            let report_path = out.join("report.md");
            std::fs::write(&report_path, &report)?;

            println!("{report}");
            eprintln!(
                "\nwrote {} and per-run JSONL to {}",
                report_path.display(),
                out.display()
            );
        }
        Command::Paper {
            out,
            provider,
            agents_sweep,
            scenarios,
            trials,
            max_samples,
            interval_ms,
            agent_budget_mib,
        } => {
            run_paper(PaperArgs {
                out,
                provider,
                agents_sweep,
                scenarios,
                trials,
                max_samples,
                interval_ms,
                agent_budget_mib,
            })?;
        }
        Command::Scenarios => {
            // The four canonical scenarios plus the explicitly-selectable `hold`/`escape` extras
            // (not in `ALL`).
            for s in Scenario::ALL.iter().copied().chain([
                Scenario::Hold,
                Scenario::Escape,
                Scenario::Overcommit,
            ]) {
                println!("{:6}  {}", s.slug(), s.description());
            }
        }
        Command::Matrix { host_mem_gib } => {
            let matrix = TestMatrix::default();
            let mem = host_mem_gib.unwrap_or_else(detect_mem_gib);
            let available: Vec<String> = builtin_launchers()
                .into_iter()
                .chain(competitor_launchers())
                .filter(|l| l.is_available())
                .map(|l| l.name().to_string())
                .collect();
            let runnable = matrix.runnable_cells(mem, &available);
            println!("full §18.2 matrix: {} cells", matrix.size());
            println!(
                "runnable on this host ({} GiB, {}): {} cells across launchers [{}]",
                mem,
                memmux_bench::matrix::current_os(),
                runnable.len(),
                available.join(", ")
            );
        }
    }
    Ok(())
}

fn parse_provider(s: &str) -> anyhow::Result<Provider> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "claude-code" | "claude" => Provider::ClaudeCode,
        "codex" => Provider::Codex,
        "gemini-cli" | "gemini" => Provider::GeminiCli,
        "opencode" => Provider::OpenCode,
        "generic" => Provider::Generic,
        other => anyhow::bail!("unknown provider '{other}'"),
    })
}

/// Parse a scenario selector: the literal `all`, a single scenario slug, or a comma list of slugs
/// (e.g. `hold,escape,overcommit`). Duplicates are preserved in declared order (the caller runs
/// each once); an unknown slug is an error.
fn parse_scenarios(s: &str) -> anyhow::Result<Vec<Scenario>> {
    if s.eq_ignore_ascii_case("all") {
        return Ok(Scenario::ALL.to_vec());
    }
    let mut out = Vec::new();
    for token in s.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        out.push(parse_one_scenario(token)?);
    }
    if out.is_empty() {
        anyhow::bail!("empty scenario list");
    }
    Ok(out)
}

/// Parse a single scenario slug.
fn parse_one_scenario(s: &str) -> anyhow::Result<Scenario> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "burst" => Scenario::Burst,
        "soak" => Scenario::Soak,
        "idle" => Scenario::Idle,
        "leak" => Scenario::Leak,
        "hold" => Scenario::Hold,
        "escape" => Scenario::Escape,
        "overcommit" => Scenario::Overcommit,
        other => anyhow::bail!("unknown scenario '{other}'"),
    })
}

/// The canonical paper N-sweep (SUM-171): {1, 5, 10, 20}.
const PAPER_AGENTS_SWEEP: &str = "1,5,10,20";
/// The canonical paper scenario set (SUM-171): hold + escape + overcommit.
const PAPER_SCENARIOS: &str = "hold,escape,overcommit";
/// The canonical paper trial count (SUM-171).
const PAPER_TRIALS: usize = 3;
/// The canonical paper max-samples per run.
const PAPER_MAX_SAMPLES: usize = 20;
/// The canonical paper sampling interval (ms) — a realistic steady-state daemon cadence.
const PAPER_INTERVAL_MS: u64 = 1000;
/// The canonical constrained overcommit agent budget (MiB): below the aggregate peak of N agents so
/// MemMux must govern (the hold/escape sweep cells stay unconstrained; the budget only applies to
/// the overcommit scenario, which `run_benchmark` enforces via `Scenario::is_overcommit`).
const PAPER_AGENT_BUDGET_MIB: u64 = 400;

/// Parsed arguments for the one-command reproducer (SUM-171 / P3).
struct PaperArgs {
    out: PathBuf,
    provider: String,
    agents_sweep: Option<String>,
    scenarios: Option<String>,
    trials: Option<usize>,
    max_samples: Option<usize>,
    interval_ms: Option<u64>,
    agent_budget_mib: Option<u64>,
}

/// Run the canonical paper matrix and write a self-contained artifact directory (SUM-171 / P3).
///
/// Preset: launchers = builtins + competitors-if-present; scenarios = `hold,escape,overcommit`;
/// N-sweep = `1,5,10,20`; trials = 3; plus a constrained overcommit budget. Every field is an
/// optional smoke override so the same command runs tiny in CI/dev. Writes all per-cell JSONL +
/// `.procs.jsonl`, `report.md` (host-spec header + all tables + figures), the SVG figures, and
/// `host.json`; prints the out-dir at the end. Deterministic in structure (stable filenames), no
/// network.
fn run_paper(args: PaperArgs) -> anyhow::Result<()> {
    let provider = parse_provider(&args.provider)?;
    let scenarios = parse_scenarios(args.scenarios.as_deref().unwrap_or(PAPER_SCENARIOS))?;
    let sweep_list = args.agents_sweep.as_deref().unwrap_or(PAPER_AGENTS_SWEEP);
    let agents_sweep = parse_agents_sweep(sweep_list)
        .map_err(|e| anyhow::anyhow!("invalid --agents-sweep '{sweep_list}': {e}"))?;
    let trials = args.trials.unwrap_or(PAPER_TRIALS);
    let max_samples = args.max_samples.unwrap_or(PAPER_MAX_SAMPLES);
    let interval_ms = args.interval_ms.unwrap_or(PAPER_INTERVAL_MS);
    let budget_mib = args.agent_budget_mib.unwrap_or(PAPER_AGENT_BUDGET_MIB);
    let agent_budget_bytes = Some(budget_mib * 1024 * 1024);

    std::fs::create_dir_all(&args.out)?;

    // Capture the host spec first, write `host.json`, and thread it into the report header.
    let host = HostSpec::detect();
    let host_json = serde_json::to_vec_pretty(&host)?;
    std::fs::write(args.out.join("host.json"), &host_json)?;

    // The canonical launcher set: builtins (raw/tmux/herdr/memmux, self-skipping when absent) plus
    // the competitor plugins (always self-skip on the reference host; recorded skipped-with-reason).
    let mut launchers: Vec<Box<dyn Launcher>> = builtin_launchers();
    launchers.extend(competitor_launchers());
    let available: Vec<&str> = launchers
        .iter()
        .filter(|l| l.is_available())
        .map(|l| l.name())
        .collect();
    eprintln!("paper: host = {} {}", host.os, host.arch);
    eprintln!("paper: available launchers: {}", available.join(", "));
    eprintln!(
        "paper: scenarios = [{}], N-sweep = {:?}, trials = {trials}, budget = {budget_mib} MiB",
        scenarios
            .iter()
            .map(|s| s.slug())
            .collect::<Vec<_>>()
            .join(", "),
        agents_sweep,
    );

    let cfg = RunConfig {
        provider,
        intensity: 1,
        interval_ms,
        max_samples,
        // `agents` is the fallback single-N; the sweep overrides it for every cell.
        agents: *agents_sweep.first().unwrap_or(&1),
        agents_sweep: Some(agents_sweep),
        trials,
        bench_exe: std::env::current_exe()?,
        workdir: args.out.clone(),
        agent_budget_bytes,
        // The paper reproducer always runs the deterministic stub workload.
        agent_cmd: None,
        agent_cwd: None,
    };

    let mut outcome = run_benchmark(&launchers, &scenarios, &cfg)?;
    for (name, reason) in &outcome.skipped {
        eprintln!("paper: skipped {name}: {reason}");
    }

    // Re-stamp the report metadata with the captured host spec so the header carries the full
    // OS/RAM/CPU block (SUM-171). Launcher-version citations are preserved.
    let versions = std::mem::take(&mut outcome.meta.launcher_versions);
    outcome.meta = ReportMeta::now_with_host(versions, host.clone());

    let report = outcome.to_markdown("MemMux paper benchmark (reproducer)");
    let report_path = args.out.join("report.md");
    std::fs::write(&report_path, &report)?;

    println!("wrote paper artifacts to {}", args.out.display());
    Ok(())
}

/// Best-effort physical memory detection in GiB (0 disables mem filtering upstream).
fn detect_mem_gib() -> u32 {
    #[cfg(target_os = "macos")]
    {
        let mut size: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        let name = std::ffi::CString::new("hw.memsize").unwrap();
        // SAFETY: `sysctlbyname` writes at most `len` bytes into `size`; we pass a null
        // new-value pointer with length 0 (a read-only query).
        let rc = unsafe {
            sysctlbyname(
                name.as_ptr(),
                &mut size as *mut u64 as *mut _,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 && size > 0 {
            return (size / (1024 * 1024 * 1024)) as u32;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    if let Ok(kb) = rest.trim().trim_end_matches("kB").trim().parse::<u64>() {
                        return (kb / (1024 * 1024)) as u32;
                    }
                }
            }
        }
    }
    32
}

#[cfg(target_os = "macos")]
extern "C" {
    fn sysctlbyname(
        name: *const std::os::raw::c_char,
        oldp: *mut std::os::raw::c_void,
        oldlenp: *mut usize,
        newp: *mut std::os::raw::c_void,
        newlen: usize,
    ) -> std::os::raw::c_int;
}
