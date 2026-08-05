//! Integration test: recursively terminate a real spawned process tree (SUM-76).

#![cfg(unix)]

use memmux_metrics::{default_sampler, terminate_subtree};
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn terminate_subtree_reaps_a_real_process_tree() {
    // A shell that spawns a background child plus a foreground child — a small owned tree.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("sleep 30 & sleep 30")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sh");
    let root = child.id() as i32;

    // Let the children come up.
    std::thread::sleep(Duration::from_millis(150));

    let sampler = default_sampler();
    let report =
        terminate_subtree(sampler.as_ref(), root, Duration::from_millis(400)).expect("terminate");

    assert!(
        report.fully_cleaned(),
        "survivors remained: {:?} (cleanup {:.3})",
        report.survivors,
        report.cleanup_fraction()
    );
    assert!(report.cleanup_fraction() >= 0.995);
    assert!(report.targeted.contains(&root));

    // Reap the shell and confirm it is gone from a fresh snapshot.
    let _ = child.wait();
    let after = default_sampler().snapshot().unwrap();
    assert!(
        !after.samples.iter().any(|s| s.pid == root),
        "root pid still present"
    );
}
