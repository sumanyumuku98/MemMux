//! Per-task reservation model (SUM-45 / §7.2).
//!
//! A reservation is the capacity the scheduler sets aside for a task *before* it launches, so
//! admission never over-commits the host.

use serde::{Deserialize, Serialize};

/// The component reservations that make up a task's memory footprint (§7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Reservation {
    /// Expected provider CLI runtime and session state.
    pub base_provider_bytes: u64,
    /// Shell, PTY, worktree watchers, wrapper overhead.
    pub isolation_bytes: u64,
    /// Expected MCP servers, browser, test/build workers.
    pub tool_profile_bytes: u64,
    /// Changed-file index and task-local metadata.
    pub repo_overlay_bytes: u64,
    /// Temporary peak during compilation, tests, or browser startup.
    pub burst_allowance_bytes: u64,
    /// Smaller reservation used when resuming a checkpointed task.
    pub resume_allowance_bytes: u64,
}

impl Reservation {
    /// Steady-state footprint: everything except the transient burst allowance.
    pub fn steady_bytes(&self) -> u64 {
        self.base_provider_bytes
            .saturating_add(self.isolation_bytes)
            .saturating_add(self.tool_profile_bytes)
            .saturating_add(self.repo_overlay_bytes)
    }

    /// Peak footprint the scheduler must be able to absorb: steady plus burst.
    pub fn peak_bytes(&self) -> u64 {
        self.steady_bytes()
            .saturating_add(self.burst_allowance_bytes)
    }

    /// Reservation for resuming a checkpointed task (base + isolation + a smaller allowance).
    pub fn resume_bytes(&self) -> u64 {
        self.base_provider_bytes
            .saturating_add(self.isolation_bytes)
            .saturating_add(self.resume_allowance_bytes)
    }

    /// Combine two reservations component-wise (e.g. task reservation + a scaled tool profile).
    pub fn saturating_add(&self, other: &Reservation) -> Reservation {
        Reservation {
            base_provider_bytes: self
                .base_provider_bytes
                .saturating_add(other.base_provider_bytes),
            isolation_bytes: self.isolation_bytes.saturating_add(other.isolation_bytes),
            tool_profile_bytes: self
                .tool_profile_bytes
                .saturating_add(other.tool_profile_bytes),
            repo_overlay_bytes: self
                .repo_overlay_bytes
                .saturating_add(other.repo_overlay_bytes),
            burst_allowance_bytes: self
                .burst_allowance_bytes
                .saturating_add(other.burst_allowance_bytes),
            resume_allowance_bytes: self
                .resume_allowance_bytes
                .saturating_add(other.resume_allowance_bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn sample() -> Reservation {
        Reservation {
            base_provider_bytes: 400 * MIB,
            isolation_bytes: 80 * MIB,
            tool_profile_bytes: 300 * MIB,
            repo_overlay_bytes: 60 * MIB,
            burst_allowance_bytes: 500 * MIB,
            resume_allowance_bytes: 120 * MIB,
        }
    }

    #[test]
    fn steady_peak_resume_relationships() {
        let r = sample();
        assert_eq!(r.steady_bytes(), (400 + 80 + 300 + 60) * MIB);
        assert_eq!(r.peak_bytes(), r.steady_bytes() + 500 * MIB);
        assert_eq!(r.resume_bytes(), (400 + 80 + 120) * MIB);
        // Peak dominates steady dominates resume for this profile.
        assert!(r.peak_bytes() > r.steady_bytes());
        assert!(r.steady_bytes() > r.resume_bytes());
    }

    #[test]
    fn component_addition_saturates() {
        let a = sample();
        let b = sample();
        let sum = a.saturating_add(&b);
        assert_eq!(sum.base_provider_bytes, 800 * MIB);
        assert_eq!(sum.peak_bytes(), a.peak_bytes() + b.peak_bytes());
    }
}
