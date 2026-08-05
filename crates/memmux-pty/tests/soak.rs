//! Terminal-flood soak tests (SUM-56 / §18.4, §18.5).
//!
//! The launch gate requires < 100 MB daemon growth over an 8-hour flood, with no growth
//! proportional to historical output. The bounded-capture pipeline is the component that
//! provides this guarantee, so we exercise it directly here:
//!
//! * a fast proxy (`flood_keeps_capture_memory_bounded`) runs on every `cargo test`; and
//! * the real 8-hour soak (`eight_hour_terminal_flood_soak`) is `#[ignore]`d by default and
//!   duration-configurable. The full end-to-end daemon soak runs once the daemon is assembled
//!   (Phase 1, SUM-12).

use memmux_pty::{CaptureBuffer, CaptureConfig};
use std::time::{Duration, Instant};

fn drain(cap: &mut CaptureBuffer) {
    // Simulate the chunk store / audit consuming evicted lines, artifacts, and events.
    cap.drain_evicted();
    cap.drain_artifacts();
    cap.drain_events();
}

#[test]
fn flood_keeps_capture_memory_bounded() {
    let cfg = CaptureConfig::default();
    let mut cap = CaptureBuffer::new(cfg);
    for i in 0..2_000_000u64 {
        cap.ingest(
            &format!("build log line {i} lorem ipsum dolor sit amet\n"),
            0,
        );
        if i % 100_000 == 0 {
            drain(&mut cap);
        }
    }
    drain(&mut cap);
    assert!(
        cap.resident_bytes() <= cfg.ring.max_bytes + 4096,
        "resident bytes {} exceeded cap {}",
        cap.resident_bytes(),
        cfg.ring.max_bytes
    );
}

#[test]
#[ignore = "long-running; set MEMMUX_SOAK_SECS and run with --ignored"]
fn eight_hour_terminal_flood_soak() {
    let secs: u64 = std::env::var("MEMMUX_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8 * 3600);
    let deadline = Instant::now() + Duration::from_secs(secs);

    let cfg = CaptureConfig::default();
    let mut cap = CaptureBuffer::new(cfg);
    let cap_bytes = cfg.ring.max_bytes;
    let mut i = 0u64;
    while Instant::now() < deadline {
        for _ in 0..10_000 {
            cap.ingest(
                &format!("soak line {i} with a representative payload of some length\n"),
                0,
            );
            i += 1;
        }
        drain(&mut cap);
        assert!(
            cap.resident_bytes() <= cap_bytes + 4096,
            "resident memory grew to {} after {} lines",
            cap.resident_bytes(),
            i
        );
    }
    eprintln!(
        "soak complete: {i} lines flooded, resident {} bytes",
        cap.resident_bytes()
    );
}
