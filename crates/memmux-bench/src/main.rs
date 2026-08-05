//! `memmux-bench` — CLI for the MemMux benchmark harness.

use clap::{Parser, Subcommand};
use memmux_bench::launcher::{builtin_launchers, competitor_launchers, Launcher};
use memmux_bench::matrix::TestMatrix;
use memmux_bench::run::{run_benchmark, RunConfig};
use memmux_bench::scenario::Scenario;
use memmux_bench::stub::{run_child_worker, SessionRecording};
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
    /// Run the benchmark across all available launchers and emit a report.
    Run {
        /// Scenario to run (`all`, `burst`, `soak`, `idle`, `leak`).
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
        /// Also run installed competitor launchers (best-effort CLI templates; see docs).
        #[arg(long, default_value_t = false)]
        include_competitors: bool,
        /// Output directory for JSONL + report.
        #[arg(long, default_value = "bench-out")]
        out: PathBuf,
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
        Command::Run {
            scenario,
            provider,
            intensity,
            interval_ms,
            max_samples,
            include_competitors,
            out,
        } => {
            let provider = parse_provider(&provider)?;
            let scenarios = parse_scenarios(&scenario)?;
            let cfg = RunConfig {
                provider,
                intensity,
                interval_ms,
                max_samples,
                bench_exe: std::env::current_exe()?,
                workdir: out.clone(),
            };
            let mut launchers: Vec<Box<dyn Launcher>> = builtin_launchers();
            if include_competitors {
                eprintln!(
                    "warning: competitor launchers use best-effort CLI templates; treat their \
numbers as indicative only (§19.5)."
                );
                launchers.extend(competitor_launchers());
            }
            let available: Vec<&str> = launchers
                .iter()
                .filter(|l| l.is_available())
                .map(|l| l.name())
                .collect();
            eprintln!("available launchers: {}", available.join(", "));

            let outcome = run_benchmark(&launchers, &scenarios, &cfg)?;
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
        Command::Scenarios => {
            for s in Scenario::ALL {
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

fn parse_scenarios(s: &str) -> anyhow::Result<Vec<Scenario>> {
    if s.eq_ignore_ascii_case("all") {
        return Ok(Scenario::ALL.to_vec());
    }
    let scenario = match s.to_ascii_lowercase().as_str() {
        "burst" => Scenario::Burst,
        "soak" => Scenario::Soak,
        "idle" => Scenario::Idle,
        "leak" => Scenario::Leak,
        other => anyhow::bail!("unknown scenario '{other}'"),
    };
    Ok(vec![scenario])
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
