//! Test matrix (SUM-39 / §18.2).
//!
//! Enumerates the benchmark axes into concrete cells. The full matrix is large and mostly not
//! runnable on a single laptop (it spans host sizes, operating systems, and competitor
//! products); this module builds the cartesian product and exposes a filter for the subset
//! that is actually executable on the current host.

use crate::scenario::Scenario;
use memmux_core::Provider;
use serde::{Deserialize, Serialize};

/// Session duration axis (§18.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Duration {
    /// 15-minute burst.
    Burst15m,
    /// 2-hour session.
    Session2h,
    /// 8-hour soak.
    Soak8h,
}

impl Duration {
    /// Approximate wall-clock seconds this duration represents.
    pub fn seconds(self) -> u64 {
        match self {
            Duration::Burst15m => 15 * 60,
            Duration::Session2h => 2 * 60 * 60,
            Duration::Soak8h => 8 * 60 * 60,
        }
    }
}

/// One fully-specified benchmark configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatrixCell {
    /// Host memory size in gibibytes (16 / 32 / 64).
    pub host_mem_gib: u32,
    /// Operating system label.
    pub os: String,
    /// Number of concurrent logical agents.
    pub agents: u32,
    /// Provider under test.
    pub provider: Provider,
    /// Scenario / workload.
    pub scenario: Scenario,
    /// Session duration.
    pub duration: Duration,
    /// Launcher / product name.
    pub product: String,
}

/// The benchmark axes (§18.2). Defaults reflect the full spec matrix.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestMatrix {
    /// Host memory sizes.
    pub host_mem_gib: Vec<u32>,
    /// Operating systems.
    pub os: Vec<String>,
    /// Agent counts.
    pub agents: Vec<u32>,
    /// Providers.
    pub providers: Vec<Provider>,
    /// Scenarios.
    pub scenarios: Vec<Scenario>,
    /// Durations.
    pub durations: Vec<Duration>,
    /// Products / launchers.
    pub products: Vec<String>,
}

impl Default for TestMatrix {
    fn default() -> Self {
        Self {
            host_mem_gib: vec![16, 32, 64],
            os: vec!["macos".into(), "linux".into()],
            agents: vec![2, 3, 5, 10],
            providers: vec![
                Provider::ClaudeCode,
                Provider::Codex,
                Provider::GeminiCli,
                Provider::OpenCode,
            ],
            scenarios: Scenario::ALL.to_vec(),
            durations: vec![Duration::Burst15m, Duration::Session2h, Duration::Soak8h],
            products: vec![
                "raw-baseline".into(),
                "memmux".into(),
                "dmux".into(),
                "cmux".into(),
                "herdr".into(),
                "agentmux".into(),
            ],
        }
    }
}

impl TestMatrix {
    /// Total number of cells in the cartesian product.
    pub fn size(&self) -> usize {
        self.host_mem_gib.len()
            * self.os.len()
            * self.agents.len()
            * self.providers.len()
            * self.scenarios.len()
            * self.durations.len()
            * self.products.len()
    }

    /// Enumerate every cell of the cartesian product.
    pub fn cells(&self) -> Vec<MatrixCell> {
        let mut cells = Vec::with_capacity(self.size());
        for &mem in &self.host_mem_gib {
            for os in &self.os {
                for &agents in &self.agents {
                    for &provider in &self.providers {
                        for &scenario in &self.scenarios {
                            for &duration in &self.durations {
                                for product in &self.products {
                                    cells.push(MatrixCell {
                                        host_mem_gib: mem,
                                        os: os.clone(),
                                        agents,
                                        provider,
                                        scenario,
                                        duration,
                                        product: product.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        cells
    }

    /// The subset of cells runnable on this host right now.
    ///
    /// A cell is runnable if its OS matches the current platform, its host-memory axis does not
    /// exceed physical memory, and `available_products` contains its product.
    pub fn runnable_cells(
        &self,
        host_mem_gib: u32,
        available_products: &[String],
    ) -> Vec<MatrixCell> {
        let os = current_os();
        self.cells()
            .into_iter()
            .filter(|c| c.os == os)
            .filter(|c| c.host_mem_gib <= host_mem_gib)
            .filter(|c| available_products.iter().any(|p| p == &c.product))
            .collect()
    }
}

/// The current OS as a matrix label.
pub fn current_os() -> String {
    if cfg!(target_os = "macos") {
        "macos".into()
    } else if cfg!(target_os = "linux") {
        "linux".into()
    } else {
        std::env::consts::OS.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_matches_enumeration() {
        let m = TestMatrix::default();
        assert_eq!(m.size(), m.cells().len());
        // 3 * 2 * 4 * 4 * 4 * 3 * 6
        assert_eq!(m.size(), 3 * 2 * 4 * 4 * 4 * 3 * 6);
    }

    #[test]
    fn runnable_filters_by_os_mem_and_product() {
        let m = TestMatrix::default();
        let runnable = m.runnable_cells(32, &["raw-baseline".into(), "memmux".into()]);
        assert!(!runnable.is_empty());
        for c in &runnable {
            assert_eq!(c.os, current_os());
            assert!(c.host_mem_gib <= 32);
            assert!(c.product == "raw-baseline" || c.product == "memmux");
        }
        // 64 GiB cells must be excluded on a 32 GiB host.
        assert!(runnable.iter().all(|c| c.host_mem_gib != 64));
    }

    #[test]
    fn no_products_means_no_runnable_cells() {
        let m = TestMatrix::default();
        assert!(m.runnable_cells(64, &[]).is_empty());
    }

    #[test]
    fn duration_seconds_are_ordered() {
        assert!(Duration::Burst15m.seconds() < Duration::Session2h.seconds());
        assert!(Duration::Session2h.seconds() < Duration::Soak8h.seconds());
    }
}
