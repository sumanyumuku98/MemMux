//! Resource-class priors and EWMA peak prediction (SUM-46 / §7.3).
//!
//! Bootstrap reservations from a static [`ResourceClass`] prior, then refine per
//! provider/repository with an exponentially-weighted estimate whose conservative bound never
//! drops below the observed stable baseline.

use crate::reservation::Reservation;
use memmux_core::ResourceClass;
use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;

/// Static reservation prior for a resource class (§7.3 bootstrap).
pub fn class_reservation(class: ResourceClass) -> Reservation {
    // Values are rough priors; the EWMA predictor refines them from real observations.
    match class {
        ResourceClass::Small => Reservation {
            base_provider_bytes: 250 * MIB,
            isolation_bytes: 60 * MIB,
            tool_profile_bytes: 40 * MIB,
            repo_overlay_bytes: 30 * MIB,
            burst_allowance_bytes: 200 * MIB,
            resume_allowance_bytes: 80 * MIB,
        },
        ResourceClass::Standard => Reservation {
            base_provider_bytes: 400 * MIB,
            isolation_bytes: 90 * MIB,
            tool_profile_bytes: 250 * MIB,
            repo_overlay_bytes: 80 * MIB,
            burst_allowance_bytes: 500 * MIB,
            resume_allowance_bytes: 150 * MIB,
        },
        ResourceClass::BrowserHeavy => Reservation {
            base_provider_bytes: 450 * MIB,
            isolation_bytes: 100 * MIB,
            tool_profile_bytes: 1200 * MIB,
            repo_overlay_bytes: 80 * MIB,
            burst_allowance_bytes: 900 * MIB,
            resume_allowance_bytes: 180 * MIB,
        },
        ResourceClass::BuildHeavy => Reservation {
            base_provider_bytes: 400 * MIB,
            isolation_bytes: 100 * MIB,
            tool_profile_bytes: 500 * MIB,
            repo_overlay_bytes: 120 * MIB,
            burst_allowance_bytes: 1500 * MIB,
            resume_allowance_bytes: 180 * MIB,
        },
        ResourceClass::Custom => Reservation {
            base_provider_bytes: 400 * MIB,
            isolation_bytes: 90 * MIB,
            tool_profile_bytes: 250 * MIB,
            repo_overlay_bytes: 80 * MIB,
            burst_allowance_bytes: 500 * MIB,
            resume_allowance_bytes: 150 * MIB,
        },
    }
}

/// Exponentially-weighted predictor of peak memory, tracking mean and variance so it can emit a
/// conservative upper bound. Never predicts below the smallest observed peak — the stable
/// baseline the task always needs (§7.3 "never reduce below observed stable baseline").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EwmaPredictor {
    alpha: f64,
    mean: Option<f64>,
    var: f64,
    baseline_floor: Option<u64>,
    samples: u64,
}

impl EwmaPredictor {
    /// Create a predictor with smoothing factor `alpha` in (0, 1]; clamped into range.
    pub fn new(alpha: f64) -> Self {
        Self {
            alpha: alpha.clamp(0.01, 1.0),
            mean: None,
            var: 0.0,
            baseline_floor: None,
            samples: 0,
        }
    }

    /// Number of observations recorded.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Record an observed peak (bytes).
    pub fn observe(&mut self, peak_bytes: u64) {
        let x = peak_bytes as f64;
        match self.mean {
            None => {
                self.mean = Some(x);
                self.var = 0.0;
            }
            Some(mean) => {
                // West's incremental EWMA mean + variance.
                let diff = x - mean;
                let incr = self.alpha * diff;
                self.mean = Some(mean + incr);
                self.var = (1.0 - self.alpha) * (self.var + diff * incr);
            }
        }
        self.baseline_floor = Some(match self.baseline_floor {
            Some(f) => f.min(peak_bytes),
            None => peak_bytes,
        });
        self.samples += 1;
    }

    /// The current EWMA mean peak, if any observations exist.
    pub fn mean_peak_bytes(&self) -> Option<u64> {
        self.mean.map(|m| m.max(0.0) as u64)
    }

    /// A conservative predicted peak: `mean + z * stddev`, floored at the observed baseline.
    ///
    /// `z` widens the bound (e.g. 2.0 ≈ ~97.5% one-sided under normal assumptions). Returns
    /// `None` until at least one observation exists.
    pub fn predicted_peak_bytes(&self, z: f64) -> Option<u64> {
        let mean = self.mean?;
        let std = self.var.max(0.0).sqrt();
        let bound = (mean + z.max(0.0) * std).max(0.0) as u64;
        Some(match self.baseline_floor {
            Some(floor) => bound.max(floor),
            None => bound,
        })
    }
}

impl Default for EwmaPredictor {
    fn default() -> Self {
        Self::new(0.3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_priors_order_by_weight() {
        assert!(
            class_reservation(ResourceClass::Small).peak_bytes()
                < class_reservation(ResourceClass::Standard).peak_bytes()
        );
        assert!(
            class_reservation(ResourceClass::BrowserHeavy).tool_profile_bytes
                > class_reservation(ResourceClass::Standard).tool_profile_bytes
        );
        assert!(
            class_reservation(ResourceClass::BuildHeavy).burst_allowance_bytes
                > class_reservation(ResourceClass::Standard).burst_allowance_bytes
        );
    }

    #[test]
    fn predictor_none_until_observed() {
        let p = EwmaPredictor::new(0.3);
        assert!(p.predicted_peak_bytes(2.0).is_none());
        assert_eq!(p.samples(), 0);
    }

    #[test]
    fn predictor_tracks_mean_and_conservative_bound() {
        let mut p = EwmaPredictor::new(0.5);
        for v in [1000u64, 1000, 1000, 1000] {
            p.observe(v);
        }
        // Stable input -> mean ~1000, variance ~0, bound ~1000.
        let mean = p.mean_peak_bytes().unwrap();
        assert!((mean as i64 - 1000).abs() <= 1);
        let pred = p.predicted_peak_bytes(2.0).unwrap();
        assert!(
            pred >= 1000,
            "conservative bound must be >= mean for stable input"
        );
    }

    #[test]
    fn predicted_peak_never_below_observed_baseline() {
        let mut p = EwmaPredictor::new(0.6);
        p.observe(2000);
        p.observe(500); // a low baseline observation
        p.observe(2200);
        // Even as the EWMA mean rises, the predictor never drops below the smallest observed.
        let pred = p.predicted_peak_bytes(0.0).unwrap();
        assert!(pred >= 500);
    }

    #[test]
    fn higher_z_widens_the_bound_under_variance() {
        let mut p = EwmaPredictor::new(0.5);
        for v in [500u64, 1500, 800, 2000, 300] {
            p.observe(v);
        }
        let low = p.predicted_peak_bytes(0.0).unwrap();
        let high = p.predicted_peak_bytes(3.0).unwrap();
        assert!(high >= low);
    }
}
