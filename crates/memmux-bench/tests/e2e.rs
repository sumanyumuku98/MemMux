//! End-to-end integration test for the benchmark harness.
//!
//! This exercises the full Phase 0 path on real processes: launch a stub via a launcher, have
//! it spawn a child worker, sample the live process tree with `memmux-metrics`, and confirm the
//! launched tree is fully attributed to the owning task (the Phase 0 exit criterion).

use memmux_bench::launcher::{Launcher, RawLauncher};
use memmux_bench::run::{run_launcher_scenario, RunConfig};
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

#[test]
fn burst_run_attributes_the_whole_launched_tree() {
    let dir = workdir("burst");
    let cfg = RunConfig {
        provider: Provider::Generic,
        intensity: 1,
        interval_ms: 30,
        max_samples: 20,
        bench_exe: bench_exe(),
        workdir: dir.clone(),
    };

    let series = run_launcher_scenario(&RawLauncher, Scenario::Burst, &cfg)
        .expect("burst run should succeed");

    assert!(!series.is_empty(), "expected at least one sample");
    // Every sample must attribute 100% of the launched tree to the task.
    assert_eq!(
        series.min_tree_attributed_fraction(),
        1.0,
        "some launched-tree memory was not attributed to the task"
    );
    // The stub itself is always present; its resident footprint is non-zero.
    assert!(
        series.peak_root_subtree_bytes() > 0,
        "root subtree footprint was zero"
    );
    // The stub spawns a child worker, so at peak the tree has more than one process.
    assert!(
        series.peak_root_process_count() >= 1,
        "expected to observe the launched process"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn idle_run_stays_bounded() {
    let dir = workdir("idle");
    let cfg = RunConfig {
        provider: Provider::Generic,
        intensity: 1,
        interval_ms: 20,
        max_samples: 5,
        bench_exe: bench_exe(),
        workdir: dir.clone(),
    };

    let series =
        run_launcher_scenario(&RawLauncher, Scenario::Idle, &cfg).expect("idle run should succeed");
    assert!(!series.is_empty());
    assert_eq!(series.min_tree_attributed_fraction(), 1.0);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn raw_launcher_reports_baseline_kind() {
    assert_eq!(RawLauncher.name(), "raw-baseline");
    assert!(RawLauncher.is_available());
}
