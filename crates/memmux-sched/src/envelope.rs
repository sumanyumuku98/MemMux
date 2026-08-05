//! Resource envelope calculation (SUM-44 / §7.1).
//!
//! ```text
//! agent_budget = physical
//!              - os_reserve
//!              - interactive_app_reserve
//!              - shared_service_reserve
//!              - swap_safety_margin
//! ```

use serde::{Deserialize, Serialize};

const GIB: u64 = 1024 * 1024 * 1024;

/// Memory carved out before any agent budget is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reserves {
    /// Kernel / OS reserve.
    pub os_bytes: u64,
    /// Reserve for the interactive app stack (IDE, browser, terminal).
    pub interactive_app_bytes: u64,
    /// Reserve for MemMux's own shared services (repo index, MCP gateway).
    pub shared_service_bytes: u64,
    /// Headroom kept free to avoid swap thrashing.
    pub swap_safety_margin_bytes: u64,
}

impl Reserves {
    /// Total reserved bytes.
    pub fn total(&self) -> u64 {
        self.os_bytes
            .saturating_add(self.interactive_app_bytes)
            .saturating_add(self.shared_service_bytes)
            .saturating_add(self.swap_safety_margin_bytes)
    }

    /// Sensible defaults scaled to the host so the budget stays positive on 16 GiB machines and
    /// generous on 64 GiB ones. Each reserve is `max(floor, fraction * physical)`.
    pub fn default_for(physical_bytes: u64) -> Self {
        let frac =
            |floor: u64, num: u64, den: u64| floor.max(physical_bytes.saturating_mul(num) / den);
        Self {
            // ~8%, min 1.5 GiB
            os_bytes: frac(3 * GIB / 2, 8, 100),
            // ~20%, min 3 GiB (IDE + browser headroom)
            interactive_app_bytes: frac(3 * GIB, 20, 100),
            // ~5%, min 1 GiB
            shared_service_bytes: frac(GIB, 5, 100),
            // ~12%, min 2 GiB
            swap_safety_margin_bytes: frac(2 * GIB, 12, 100),
        }
    }
}

/// The global budget available to agent workloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceEnvelope {
    /// Total physical memory.
    pub physical_bytes: u64,
    /// Reserves subtracted from physical memory.
    pub reserves: Reserves,
    /// Memory available to agents (`physical - reserves`, saturating at 0).
    pub agent_budget_bytes: u64,
}

impl ResourceEnvelope {
    /// Build an envelope from physical memory and explicit reserves.
    pub fn new(physical_bytes: u64, reserves: Reserves) -> Self {
        let agent_budget_bytes = physical_bytes.saturating_sub(reserves.total());
        Self {
            physical_bytes,
            reserves,
            agent_budget_bytes,
        }
    }

    /// Build an envelope using the default reserves for the host size.
    pub fn with_default_reserves(physical_bytes: u64) -> Self {
        Self::new(physical_bytes, Reserves::default_for(physical_bytes))
    }

    /// Remaining budget given current agent usage (may be negative if over budget).
    pub fn headroom_bytes(&self, used_bytes: u64) -> i64 {
        self.agent_budget_bytes as i64 - used_bytes as i64
    }

    /// Fraction of the agent budget currently used (0.0 for a zero budget).
    pub fn utilization(&self, used_bytes: u64) -> f64 {
        if self.agent_budget_bytes == 0 {
            return if used_bytes == 0 { 0.0 } else { 1.0 };
        }
        used_bytes as f64 / self.agent_budget_bytes as f64
    }

    /// Whether `additional_bytes` would fit within the budget on top of `used_bytes`.
    pub fn fits(&self, used_bytes: u64, additional_bytes: u64) -> bool {
        used_bytes.saturating_add(additional_bytes) <= self.agent_budget_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_positive_and_monotonic_across_host_sizes() {
        let b16 = ResourceEnvelope::with_default_reserves(16 * GIB).agent_budget_bytes;
        let b32 = ResourceEnvelope::with_default_reserves(32 * GIB).agent_budget_bytes;
        let b64 = ResourceEnvelope::with_default_reserves(64 * GIB).agent_budget_bytes;
        assert!(b16 > 0, "16 GiB host should still have a positive budget");
        assert!(b32 > b16 && b64 > b32, "budget must grow with host memory");
        // 32 GiB host lands in a plausible range (spec Appendix A shows ~15 GiB).
        assert!(b32 > 12 * GIB && b32 < 22 * GIB, "32 GiB budget was {b32}");
    }

    #[test]
    fn explicit_reserves_subtract_exactly() {
        let reserves = Reserves {
            os_bytes: 2 * GIB,
            interactive_app_bytes: 4 * GIB,
            shared_service_bytes: GIB,
            swap_safety_margin_bytes: 3 * GIB,
        };
        let env = ResourceEnvelope::new(32 * GIB, reserves);
        assert_eq!(env.agent_budget_bytes, (32 - 10) * GIB);
    }

    #[test]
    fn headroom_utilization_and_fit() {
        let env = ResourceEnvelope::new(
            20 * GIB,
            Reserves {
                os_bytes: 0,
                interactive_app_bytes: 0,
                shared_service_bytes: 0,
                swap_safety_margin_bytes: 0,
            },
        );
        assert_eq!(env.agent_budget_bytes, 20 * GIB);
        assert_eq!(env.headroom_bytes(5 * GIB), 15 * GIB as i64);
        assert!((env.utilization(10 * GIB) - 0.5).abs() < 1e-9);
        assert!(env.fits(10 * GIB, 10 * GIB));
        assert!(!env.fits(10 * GIB, 11 * GIB));
        assert!(env.headroom_bytes(25 * GIB) < 0);
    }

    #[test]
    fn oversized_reserves_saturate_budget_to_zero() {
        let env = ResourceEnvelope::new(4 * GIB, Reserves::default_for(64 * GIB));
        assert_eq!(env.agent_budget_bytes, 0);
        assert_eq!(env.utilization(0), 0.0);
        assert_eq!(env.utilization(1), 1.0);
    }
}
