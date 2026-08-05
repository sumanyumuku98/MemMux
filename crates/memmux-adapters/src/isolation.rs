//! Adapter isolation & capability grants (SUM-75 / §12.3).
//!
//! Adapters — especially third-party ones — get only the repository, process, and secret
//! capabilities they need. Running untrusted adapters *out of process* under a restricted
//! plugin runtime is the Phase-5 SDK's job; this module models the least-privilege grant the
//! daemon enforces around any adapter today.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The capabilities granted to a running adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGrant {
    /// Filesystem roots the adapter may access (typically just its task worktree).
    pub repo_paths: Vec<PathBuf>,
    /// Whether the adapter may use the network.
    pub allow_network: bool,
    /// Secret *references* the adapter may resolve (never plaintext secrets).
    pub secret_refs: Vec<String>,
    /// Whether the adapter must run out of process (required for third-party code).
    pub out_of_process: bool,
}

impl CapabilityGrant {
    /// A least-privilege grant scoped to a single worktree: no network, no secrets, in-process
    /// (first-party adapters). Third-party adapters should set `out_of_process`.
    pub fn least_privilege(worktree: impl Into<PathBuf>) -> Self {
        Self {
            repo_paths: vec![worktree.into()],
            allow_network: false,
            secret_refs: Vec::new(),
            out_of_process: false,
        }
    }

    /// Whether `path` is within a granted repository root.
    pub fn permits_path(&self, path: &Path) -> bool {
        self.repo_paths.iter().any(|root| path.starts_with(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn least_privilege_is_scoped_and_closed() {
        let grant = CapabilityGrant::least_privilege("/wt/task-1");
        assert!(!grant.allow_network);
        assert!(grant.secret_refs.is_empty());
        assert!(grant.permits_path(Path::new("/wt/task-1/src/main.rs")));
        assert!(!grant.permits_path(Path::new("/etc/passwd")));
        assert!(!grant.permits_path(Path::new("/wt/task-2/secret")));
    }
}
