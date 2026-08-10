//! Pure descriptive statistics for the multi-trial runner (SUM-162).
//!
//! Hand-rolled (no external crates) mean / median / sample standard deviation and a 95% confidence
//! interval half-width across the K per-trial values of a headline metric, so the paper can report
//! `mean ±halfwidth` instead of a single unqualified number.

use serde::{Deserialize, Serialize};

/// The z-score for a two-sided 95% confidence interval under a normal approximation.
const Z_95: f64 = 1.96;

/// Summary statistics over the K per-trial values of one headline metric.
///
/// `stddev` is the **sample** standard deviation (Bessel's `n-1` denominator). `ci95_halfwidth`
/// is the 95% confidence-interval half-width of the mean, `1.96 * stddev / sqrt(n)`; report the
/// metric as `mean ± ci95_halfwidth`. For `n < 2` the spread is undefined, so `stddev` and
/// `ci95_halfwidth` are `0.0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrialStats {
    /// Arithmetic mean of the values.
    pub mean: f64,
    /// Median (middle value; average of the two middle values for even `n`).
    pub median: f64,
    /// Sample standard deviation (`n-1` denominator); `0.0` when `n < 2`.
    pub stddev: f64,
    /// 95% confidence-interval half-width of the mean; `0.0` when `n < 2`.
    pub ci95_halfwidth: f64,
    /// Number of values summarized.
    pub n: usize,
}

/// Summarize a slice of values into a [`TrialStats`].
///
/// Handles the degenerate sizes the multi-trial runner can produce:
/// * `n == 0` → all fields `0.0` (an all-zero stat, so an empty run is honest, not a panic).
/// * `n == 1` → `mean == median == value`, `stddev == 0.0`, `ci95_halfwidth == 0.0`.
///
/// The median averages the two middle values for even `n`. `stddev` uses the sample (`n-1`)
/// denominator and `ci95_halfwidth = 1.96 * stddev / sqrt(n)`.
pub fn summarize(values: &[f64]) -> TrialStats {
    let n = values.len();
    if n == 0 {
        return TrialStats::default();
    }
    let mean = values.iter().sum::<f64>() / n as f64;

    // Median on a sorted copy (does not mutate the caller's data).
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    };

    if n < 2 {
        return TrialStats {
            mean,
            median,
            stddev: 0.0,
            ci95_halfwidth: 0.0,
            n,
        };
    }

    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    let stddev = variance.sqrt();
    let ci95_halfwidth = Z_95 * stddev / (n as f64).sqrt();

    TrialStats {
        mean,
        median,
        stddev,
        ci95_halfwidth,
        n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector_matches_hand_checked_values() {
        // values: [2, 4, 4, 4, 5, 5, 7, 9], mean = 5.0.
        // Sample variance (n-1=7): sum of squared deviations = 9+1+1+1+0+0+4+16 = 32; 32/7 ≈
        // 4.5714286; stddev ≈ 2.1380899. n=8 (even) → median = (4+5)/2 = 4.5.
        // ci95 = 1.96 * 2.1380899 / sqrt(8) = 1.96 * 2.1380899 / 2.8284271 ≈ 1.4816207.
        let s = summarize(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert_eq!(s.n, 8);
        assert!((s.mean - 5.0).abs() < 1e-9);
        assert!((s.median - 4.5).abs() < 1e-9);
        assert!((s.stddev - 2.138089935).abs() < 1e-6);
        assert!((s.ci95_halfwidth - 1.4816207).abs() < 1e-5);
    }

    #[test]
    fn odd_n_median_is_the_middle_value() {
        let s = summarize(&[10.0, 2.0, 6.0]);
        assert_eq!(s.n, 3);
        assert!((s.mean - 6.0).abs() < 1e-9);
        assert!((s.median - 6.0).abs() < 1e-9);
        // Sample variance (n-1=2): (16+16+0)/2 = 16 → stddev 4.0.
        assert!((s.stddev - 4.0).abs() < 1e-9);
        assert!((s.ci95_halfwidth - (Z_95 * 4.0 / (3.0f64).sqrt())).abs() < 1e-9);
    }

    #[test]
    fn n_one_has_zero_spread() {
        let s = summarize(&[42.0]);
        assert_eq!(s.n, 1);
        assert!((s.mean - 42.0).abs() < 1e-9);
        assert!((s.median - 42.0).abs() < 1e-9);
        assert_eq!(s.stddev, 0.0);
        assert_eq!(s.ci95_halfwidth, 0.0);
    }

    #[test]
    fn n_zero_is_all_zero() {
        let s = summarize(&[]);
        assert_eq!(s, TrialStats::default());
        assert_eq!(s.n, 0);
        assert_eq!(s.mean, 0.0);
        assert_eq!(s.median, 0.0);
        assert_eq!(s.stddev, 0.0);
        assert_eq!(s.ci95_halfwidth, 0.0);
    }

    #[test]
    fn identical_values_have_zero_stddev() {
        let s = summarize(&[3.0, 3.0, 3.0, 3.0]);
        assert!((s.mean - 3.0).abs() < 1e-9);
        assert_eq!(s.stddev, 0.0);
        assert_eq!(s.ci95_halfwidth, 0.0);
    }
}
