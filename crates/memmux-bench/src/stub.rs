//! Deterministic stub coding-agent (SUM-32).
//!
//! A real coding agent's memory and output behaviour is noisy and provider-specific. For
//! reproducible benchmarks we drive a *stub* whose behaviour is a fully deterministic script
//! ([`SessionRecording`]). The same recording can be:
//!
//! * **simulated** — [`SessionRecording::simulate`] computes the exact resident-memory and
//!   output trajectory with no side effects, for unit tests and expected-value baselines; and
//! * **executed** — [`SessionRecording::execute`] actually allocates memory, emits output, and
//!   sleeps, so the harness can measure a live process with `memmux-metrics`.

use memmux_core::Provider;
use serde::{Deserialize, Serialize};

/// Bytes in one mebibyte.
pub const MIB: u64 = 1024 * 1024;

/// One scripted action in a stub session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Step {
    /// Grow resident memory by `mib` mebibytes.
    Allocate {
        /// Mebibytes to allocate.
        mib: u64,
    },
    /// Release up to `mib` mebibytes of previously allocated memory.
    Free {
        /// Mebibytes to free.
        mib: u64,
    },
    /// Emit terminal output: `lines` lines of `line_bytes` bytes each (terminal-flood driver).
    Emit {
        /// Number of lines.
        lines: u64,
        /// Bytes per line.
        line_bytes: u64,
    },
    /// Leak memory: grow resident by `mib_per_tick` for `ticks` ticks and never free it.
    Leak {
        /// Mebibytes leaked each tick.
        mib_per_tick: u64,
        /// Number of ticks.
        ticks: u64,
    },
    /// Idle for `ms` milliseconds.
    Sleep {
        /// Milliseconds to sleep.
        ms: u64,
    },
    /// Spawn a child worker process that holds `mib` mebibytes for `hold_ms` milliseconds.
    ///
    /// This models an agent spawning a test runner / language server so the attribution engine
    /// has a real multi-process tree to map back to the owning task.
    SpawnChild {
        /// Mebibytes the child holds resident.
        mib: u64,
        /// How long the child lives, in milliseconds.
        hold_ms: u64,
    },
}

/// A named, provider-tagged deterministic session script.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecording {
    /// Human-readable name.
    pub name: String,
    /// Provider this recording stands in for.
    pub provider: Provider,
    /// Baseline resident memory the agent starts with, in mebibytes.
    pub base_mib: u64,
    /// Ordered script.
    pub steps: Vec<Step>,
}

/// One point in a simulated trajectory (after applying the step at `step`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryPoint {
    /// Index of the step just applied.
    pub step: usize,
    /// Modeled resident memory after the step, in mebibytes.
    pub resident_mib: u64,
    /// Cumulative bytes emitted to output so far.
    pub emitted_bytes: u64,
}

/// The full modeled trajectory of a [`SessionRecording`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trajectory {
    /// Points, one per step.
    pub points: Vec<TrajectoryPoint>,
    /// Starting resident memory, in mebibytes.
    pub base_mib: u64,
}

impl Trajectory {
    /// Peak modeled resident memory across the whole trajectory (including the baseline).
    pub fn peak_resident_mib(&self) -> u64 {
        self.points
            .iter()
            .map(|p| p.resident_mib)
            .max()
            .unwrap_or(self.base_mib)
            .max(self.base_mib)
    }

    /// Final modeled resident memory.
    pub fn final_resident_mib(&self) -> u64 {
        self.points
            .last()
            .map(|p| p.resident_mib)
            .unwrap_or(self.base_mib)
    }

    /// Total bytes emitted to output.
    pub fn total_emitted_bytes(&self) -> u64 {
        self.points.last().map(|p| p.emitted_bytes).unwrap_or(0)
    }

    /// Whether resident memory grows monotonically and ends above the baseline.
    ///
    /// This is the signature of a leak (§18.4 "Leak injection") and what the leak detector
    /// (Phase 3, SUM-100) must catch.
    pub fn is_monotonic_growth(&self) -> bool {
        let mut prev = self.base_mib;
        let mut ever_grew = false;
        for p in &self.points {
            if p.resident_mib < prev {
                return false;
            }
            if p.resident_mib > prev {
                ever_grew = true;
            }
            prev = p.resident_mib;
        }
        ever_grew && self.final_resident_mib() > self.base_mib
    }
}

impl SessionRecording {
    /// Create an empty recording.
    pub fn new(name: impl Into<String>, provider: Provider, base_mib: u64) -> Self {
        Self {
            name: name.into(),
            provider,
            base_mib,
            steps: Vec::new(),
        }
    }

    /// Builder: append a step.
    pub fn with(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }

    /// Compute the modeled trajectory without side effects.
    pub fn simulate(&self) -> Trajectory {
        let mut resident = self.base_mib;
        let mut emitted = 0u64;
        let mut points = Vec::with_capacity(self.steps.len());
        for (i, step) in self.steps.iter().enumerate() {
            match step {
                Step::Allocate { mib } => resident = resident.saturating_add(*mib),
                Step::Free { mib } => resident = resident.saturating_sub(*mib),
                Step::Emit { lines, line_bytes } => {
                    // Terminal output must NOT change resident memory — the multiplexer is
                    // responsible for bounding it. The stub only accumulates emitted bytes.
                    emitted = emitted.saturating_add(lines.saturating_mul(*line_bytes));
                }
                Step::Leak {
                    mib_per_tick,
                    ticks,
                } => {
                    resident = resident.saturating_add(mib_per_tick.saturating_mul(*ticks));
                }
                Step::Sleep { .. } => {}
                // A child runs in its own process; it does not change *this* process's
                // resident model. Its memory is observed live via the process tree.
                Step::SpawnChild { .. } => {}
            }
            points.push(TrajectoryPoint {
                step: i,
                resident_mib: resident,
                emitted_bytes: emitted,
            });
        }
        Trajectory {
            points,
            base_mib: self.base_mib,
        }
    }

    /// Execute the recording for real: allocate memory, emit output, and sleep.
    ///
    /// Writes emitted output to `out`. Returns the peak resident mebibytes the ballast reached
    /// (a lower bound on the process RSS delta). Intended to be run inside the `stub`
    /// subcommand as its own process so `memmux-metrics` can sample it.
    pub fn execute(&self, out: &mut impl std::io::Write) -> std::io::Result<u64> {
        use std::time::Duration;
        let mut ballast = Ballast::default();
        ballast.grow_mib(self.base_mib);
        let mut peak = ballast.resident_mib();
        let mut children: Vec<std::process::Child> = Vec::new();
        let line = |n: u64| -> Vec<u8> {
            let mut v = vec![b'x'; n.saturating_sub(1) as usize];
            v.push(b'\n');
            v
        };
        for step in &self.steps {
            match step {
                Step::Allocate { mib } => ballast.grow_mib(*mib),
                Step::Free { mib } => ballast.shrink_mib(*mib),
                Step::Emit { lines, line_bytes } => {
                    let bytes = line((*line_bytes).max(1));
                    for _ in 0..*lines {
                        out.write_all(&bytes)?;
                    }
                }
                Step::Leak {
                    mib_per_tick,
                    ticks,
                } => {
                    for _ in 0..*ticks {
                        ballast.grow_mib(*mib_per_tick);
                    }
                }
                Step::Sleep { ms } => std::thread::sleep(Duration::from_millis(*ms)),
                Step::SpawnChild { mib, hold_ms } => {
                    if let Some(child) = spawn_child_worker(*mib, *hold_ms) {
                        children.push(child);
                    }
                }
            }
            peak = peak.max(ballast.resident_mib());
        }
        // Own what you launch (§2.4): wait for every child before exiting.
        for mut child in children {
            let _ = child.wait();
        }
        Ok(peak)
    }
}

/// Spawn a `memmux-bench stub-child` worker process. Returns `None` if the executable path is
/// unavailable (in which case the step is skipped rather than failing the run).
fn spawn_child_worker(mib: u64, hold_ms: u64) -> Option<std::process::Child> {
    let exe = std::env::current_exe().ok()?;
    std::process::Command::new(exe)
        .arg("stub-child")
        .arg("--mib")
        .arg(mib.to_string())
        .arg("--hold-ms")
        .arg(hold_ms.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}

/// Run the child-worker behaviour: hold `mib` mebibytes resident for `hold_ms` then exit. Used
/// by the `stub-child` subcommand.
pub fn run_child_worker(mib: u64, hold_ms: u64) {
    let mut ballast = Ballast::default();
    ballast.grow_mib(mib);
    std::thread::sleep(std::time::Duration::from_millis(hold_ms));
    drop(ballast);
}

/// A block of touched heap memory that is guaranteed to be resident.
#[derive(Default)]
struct Ballast {
    blocks: Vec<Vec<u8>>,
}

impl Ballast {
    /// Allocate `mib` mebibytes and touch every page so it counts as resident.
    fn grow_mib(&mut self, mib: u64) {
        for _ in 0..mib {
            let mut block = vec![0u8; MIB as usize];
            // Touch each 4 KiB page so the pages are actually faulted in.
            let mut i = 0;
            while i < block.len() {
                block[i] = 1;
                i += 4096;
            }
            self.blocks.push(block);
        }
    }

    /// Release up to `mib` mebibytes.
    fn shrink_mib(&mut self, mib: u64) {
        for _ in 0..mib {
            if self.blocks.pop().is_none() {
                break;
            }
        }
    }

    /// Current ballast size in mebibytes.
    fn resident_mib(&self) -> u64 {
        self.blocks.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_then_free_tracks_resident() {
        let rec = SessionRecording::new("t", Provider::Generic, 100)
            .with(Step::Allocate { mib: 50 })
            .with(Step::Allocate { mib: 30 })
            .with(Step::Free { mib: 20 });
        let traj = rec.simulate();
        assert_eq!(traj.final_resident_mib(), 160);
        assert_eq!(traj.peak_resident_mib(), 180);
    }

    #[test]
    fn emit_does_not_change_resident_but_counts_bytes() {
        let rec = SessionRecording::new("flood", Provider::ClaudeCode, 200).with(Step::Emit {
            lines: 1000,
            line_bytes: 80,
        });
        let traj = rec.simulate();
        assert_eq!(traj.final_resident_mib(), 200);
        assert_eq!(traj.total_emitted_bytes(), 80_000);
        assert!(!traj.is_monotonic_growth());
    }

    #[test]
    fn leak_is_detected_as_monotonic_growth() {
        let rec = SessionRecording::new("leak", Provider::Codex, 100)
            .with(Step::Leak {
                mib_per_tick: 10,
                ticks: 5,
            })
            .with(Step::Sleep { ms: 0 });
        let traj = rec.simulate();
        assert_eq!(traj.final_resident_mib(), 150);
        assert!(traj.is_monotonic_growth());
    }

    #[test]
    fn free_below_zero_saturates() {
        let rec = SessionRecording::new("t", Provider::Generic, 10).with(Step::Free { mib: 100 });
        assert_eq!(rec.simulate().final_resident_mib(), 0);
    }

    #[test]
    fn recording_round_trips_through_json() {
        let rec = SessionRecording::new("t", Provider::GeminiCli, 50)
            .with(Step::Allocate { mib: 10 })
            .with(Step::Emit {
                lines: 5,
                line_bytes: 20,
            });
        let json = serde_json::to_string(&rec).unwrap();
        let back: SessionRecording = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn execute_emits_expected_bytes() {
        let rec = SessionRecording::new("t", Provider::Generic, 0).with(Step::Emit {
            lines: 3,
            line_bytes: 4,
        });
        let mut buf = Vec::new();
        rec.execute(&mut buf).unwrap();
        // 3 lines * 4 bytes (3 'x' + newline).
        assert_eq!(buf.len(), 12);
        assert_eq!(buf.iter().filter(|&&b| b == b'\n').count(), 3);
    }
}
