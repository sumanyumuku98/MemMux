//! Adapter isolation & capability grants (SUM-75 / §12.3).
//!
//! Adapters — especially third-party ones — get only the repository, process, and secret
//! capabilities they need. Running untrusted adapters *out of process* under a restricted
//! plugin runtime is the Phase-5 SDK's job; this module models the least-privilege grant the
//! daemon enforces around any adapter today.

use memmux_lifecycle::{SecretRef, SecretSource};
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

    /// A grant that additionally permits the named secret references (SUM-79).
    pub fn with_secrets(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.secret_refs = names.into_iter().map(Into::into).collect();
        self
    }

    /// Whether `path` is within a granted repository root.
    pub fn permits_path(&self, path: &Path) -> bool {
        self.repo_paths.iter().any(|root| path.starts_with(root))
    }

    /// Whether the adapter may resolve the secret named `name`.
    pub fn permits_secret(&self, name: &str) -> bool {
        self.secret_refs.iter().any(|n| n == name)
    }

    /// Resolve the launch environment for `refs`, but **only** those this grant permits (SUM-79).
    ///
    /// Each permitted reference is read from its [`SecretSource`] (host env var or file). Refs the
    /// grant does not allow, or whose value is missing, are silently skipped — a task never
    /// receives a secret it wasn't granted, and a missing secret is not fatal here (the provider
    /// surfaces its own auth error). Values are returned for direct hand-off to the PTY launch and
    /// are never logged.
    pub fn resolve_env(&self, refs: &[SecretRef]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for r in refs {
            if !self.permits_secret(&r.name) {
                continue;
            }
            let value = match &r.source {
                SecretSource::Env { var } => std::env::var(var).ok(),
                SecretSource::File { path } => std::fs::read_to_string(path)
                    .ok()
                    .map(|s| s.trim_end_matches(['\n', '\r']).to_string()),
                // Keychain resolution is platform-specific and deferred; treat as unavailable.
                SecretSource::Keychain { .. } => None,
            };
            if let Some(value) = value {
                out.push((r.name.clone(), value));
            }
        }
        out
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

    #[test]
    fn resolve_env_only_returns_granted_secrets() {
        // Use a process-unique var name so the test is hermetic.
        let granted = format!("MEMMUX_TEST_SECRET_{}", std::process::id());
        let denied = format!("MEMMUX_TEST_DENIED_{}", std::process::id());
        std::env::set_var(&granted, "s3cr3t");
        std::env::set_var(&denied, "nope");

        let grant = CapabilityGrant::least_privilege("/wt/task-1").with_secrets([granted.clone()]);
        let refs = vec![
            SecretRef::env(granted.clone()),
            SecretRef::env(denied.clone()),
        ];
        let env = grant.resolve_env(&refs);

        // Only the granted ref is resolved; the denied one is dropped.
        assert_eq!(env.len(), 1);
        assert_eq!(env[0], (granted.clone(), "s3cr3t".to_string()));
        assert!(grant.permits_secret(&granted));
        assert!(!grant.permits_secret(&denied));

        std::env::remove_var(&granted);
        std::env::remove_var(&denied);
    }

    #[test]
    fn missing_secret_is_skipped_not_fatal() {
        let name = format!("MEMMUX_TEST_ABSENT_{}", std::process::id());
        let grant = CapabilityGrant::least_privilege("/wt/x").with_secrets([name.clone()]);
        // Granted but not present in the environment -> resolves to nothing, no panic.
        assert!(grant.resolve_env(&[SecretRef::env(name)]).is_empty());
    }
}
