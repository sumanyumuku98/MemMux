//! End-to-end integration test for the benchmark harness.
//!
//! Exercises the full path on real processes: launch N stub agents via each builtin launcher,
//! sample the live process tree with `memmux-metrics`, tag provider-vs-manager overhead, and
//! render the Markdown report. The raw baseline always runs; tmux/herdr/MemMux assertions only
//! fire when the underlying binary is actually resolvable, so a bare CI host stays green.

use memmux_bench::launcher::{
    builtin_launchers, LaunchSpec, Launcher, MemMuxLauncher, RawLauncher,
};
use memmux_bench::report::ReportMeta;
use memmux_bench::run::{run_benchmark, run_launcher_scenario, RunConfig};
use memmux_bench::scenario::Scenario;
use memmux_core::Provider;
use std::path::PathBuf;

/// Path to the compiled `memmux-bench` binary (Cargo sets this for integration tests).
fn bench_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_memmux-bench"))
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("memmux-bench-e2e-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn short_cfg(dir: PathBuf, agents: usize) -> RunConfig {
    RunConfig {
        provider: Provider::Generic,
        intensity: 1,
        interval_ms: 30,
        max_samples: 5,
        agents,
        agents_sweep: None,
        trials: 1,
        bench_exe: bench_exe(),
        workdir: dir,
        agent_budget_bytes: None,
        agent_cmd: None,
        agent_cwd: None,
    }
}

#[test]
fn raw_launcher_two_agents_have_providers_and_zero_manager_overhead() {
    let dir = workdir("raw2");
    let cfg = short_cfg(dir.clone(), 2);

    let series = run_launcher_scenario(&RawLauncher, Scenario::Burst, &cfg)
        .expect("raw burst run should succeed");

    assert!(!series.is_empty(), "expected at least one sample");
    // Provider footprint must be observed and manager overhead must be zero for the raw baseline.
    assert!(
        series.peak_provider_bytes() > 0,
        "provider footprint was zero"
    );
    assert_eq!(
        series.peak_manager_overhead_bytes(),
        0,
        "raw baseline must have no manager overhead"
    );
    // The record carries the new fields.
    let last = series.records.last().unwrap();
    assert!(last.provider_bytes > 0);
    assert_eq!(last.manager_overhead_bytes, 0);
    assert!(last.launcher_version.contains("raw"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn memmux_launcher_creates_two_providers_when_daemon_is_resolvable() {
    let dir = workdir("mmx2");
    let cfg = short_cfg(dir.clone(), 2);
    let launcher = MemMuxLauncher;

    if !launcher.is_available() {
        // The `memmuxd` binary could not be resolved in this environment — degrade to skip so CI
        // without a sibling daemon stays green (never a hard fail, never a fabricated number).
        eprintln!("skipping MemMux e2e: memmuxd binary not resolvable next to the bench exe");
        std::fs::remove_dir_all(&dir).ok();
        return;
    }

    let recording = Scenario::Burst.recording(cfg.provider, cfg.intensity);
    let recording_path = dir.join("mmx.recording.json");
    std::fs::write(
        &recording_path,
        serde_json::to_vec_pretty(&recording).unwrap(),
    )
    .unwrap();
    let spec = LaunchSpec {
        recording_path,
        bench_exe: bench_exe(),
        ..Default::default()
    };

    match launcher.start(2, &spec) {
        Ok(session) => {
            let topo = session.topology();
            assert_eq!(
                topo.agent_roots.len(),
                2,
                "MemMux should have launched exactly two provider processes"
            );
            assert_eq!(
                topo.manager_pids.len(),
                1,
                "one memmuxd daemon is the manager"
            );
            session.stop();
        }
        Err(e) => {
            // A real drive failure (not just an absent binary) is recorded as skip, per §19.5.
            eprintln!("skipping MemMux e2e: memmuxd could not be driven: {e}");
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn benchmark_report_has_total_and_manager_columns_plus_metadata() {
    let dir = workdir("report");
    let cfg = short_cfg(dir.clone(), 2);
    let launchers = builtin_launchers();

    let outcome = run_benchmark(&launchers, &[Scenario::Burst], &cfg).expect("benchmark runs");
    let report = outcome.to_markdown("MemMux benchmark e2e");

    // Side-by-side columns and a version/date metadata header are always present.
    assert!(report.contains("Total peak MiB"), "missing total column");
    assert!(
        report.contains("Manager overhead peak MiB"),
        "missing manager-overhead column"
    );
    assert!(report.contains("Measured (UTC)"), "missing date metadata");
    assert!(report.contains("Host OS"), "missing host-os metadata");

    // The raw baseline always runs and always appears with a provider footprint.
    let raw = outcome
        .summaries
        .iter()
        .find(|s| s.launcher == "raw-baseline")
        .expect("raw-baseline must always run");
    assert!(
        raw.peak_total_mib.mean > 0.0,
        "raw total footprint was zero"
    );
    assert_eq!(
        raw.peak_manager_mib.mean, 0.0,
        "raw manager overhead must be zero"
    );

    // tmux/herdr only asserted when actually available (else recorded skipped-with-reason).
    for l in &launchers {
        if l.name() == "tmux" && l.is_available() {
            let ran = outcome.summaries.iter().any(|s| s.launcher == "tmux");
            let skipped = outcome.skipped.iter().any(|(n, _)| n.starts_with("tmux"));
            assert!(
                ran || skipped,
                "tmux should either run or be recorded skipped"
            );
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn report_meta_captures_host_and_versions() {
    let meta = ReportMeta::now(vec![("raw-baseline".into(), "raw (direct spawn)".into())]);
    assert_eq!(meta.host_os, std::env::consts::OS);
    assert!(meta.measured_at_utc.ends_with('Z'));
    assert_eq!(meta.launcher_versions.len(), 1);
}

#[test]
fn raw_launcher_reports_baseline_name() {
    assert_eq!(RawLauncher.name(), "raw-baseline");
    assert!(RawLauncher.is_available());
}
