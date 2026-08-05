//! Dependency & cache sharing strategy (SUM-60 / §9.3).
//!
//! Package *caches* (npm, pip, cargo) are safe to share at the user level and cut duplication
//! across worktrees; project *dependency directories* that tools mutate stay per-worktree. This
//! module turns a repo policy into an environment/plan the launcher applies — no filesystem
//! mutation here, so it is pure and testable.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Whether a given store is shared at the user level or kept per-worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShareMode {
    /// Share a single user-level store across worktrees (safe for read-mostly caches).
    Shared,
    /// Keep a private copy per worktree (required when tools mutate the store).
    PerWorktree,
}

/// Per-repository cache-sharing policy (Appendix C `bootstrap`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePolicy {
    /// Node package cache (`npm`/`pnpm`/`yarn`).
    pub node_store: ShareMode,
    /// Python wheel cache (`pip`).
    pub python_wheel_cache: ShareMode,
    /// Rust cargo registry cache.
    pub cargo_registry: ShareMode,
}

impl Default for CachePolicy {
    fn default() -> Self {
        // Read-mostly caches shared by default; project deps remain per-worktree implicitly.
        Self {
            node_store: ShareMode::Shared,
            python_wheel_cache: ShareMode::Shared,
            cargo_registry: ShareMode::Shared,
        }
    }
}

/// The concrete plan a launcher applies for a worktree.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePlan {
    /// Environment variables pointing tools at shared caches.
    pub shared_env: Vec<(String, String)>,
    /// Human-readable notes about stores kept per-worktree.
    pub per_worktree: Vec<String>,
}

impl CachePolicy {
    /// Build the cache plan given the user's shared-cache root (e.g. `~/.memmux/cache`).
    pub fn plan(&self, shared_cache_root: &Path) -> CachePlan {
        let mut shared_env = Vec::new();
        let mut per_worktree = Vec::new();
        let dir = |name: &str| shared_cache_root.join(name).to_string_lossy().into_owned();

        match self.node_store {
            ShareMode::Shared => shared_env.push(("npm_config_cache".into(), dir("npm"))),
            ShareMode::PerWorktree => per_worktree.push("node package cache".into()),
        }
        match self.python_wheel_cache {
            ShareMode::Shared => shared_env.push(("PIP_CACHE_DIR".into(), dir("pip"))),
            ShareMode::PerWorktree => per_worktree.push("python wheel cache".into()),
        }
        match self.cargo_registry {
            ShareMode::Shared => shared_env.push(("CARGO_HOME".into(), dir("cargo"))),
            ShareMode::PerWorktree => per_worktree.push("cargo registry".into()),
        }
        CachePlan {
            shared_env,
            per_worktree,
        }
    }
}

/// Default watcher ignore globs (Appendix C `ignore`) so worktree watchers skip generated dirs.
pub fn default_watch_ignores() -> Vec<&'static str> {
    vec![
        "**/.git/**",
        "**/node_modules/**",
        "**/.next/**",
        "**/dist/**",
        "**/build/**",
        "**/coverage/**",
        "**/.venv/**",
        "**/target/**",
    ]
}

/// Convenience: the shared cache root under a managed layout root.
pub fn shared_cache_root(managed_root: &Path) -> PathBuf {
    managed_root.join("cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_stores_yield_env_pointers() {
        let plan = CachePolicy::default().plan(Path::new("/home/u/.memmux/cache"));
        let keys: Vec<&str> = plan.shared_env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"npm_config_cache"));
        assert!(keys.contains(&"PIP_CACHE_DIR"));
        assert!(keys.contains(&"CARGO_HOME"));
        assert!(plan.per_worktree.is_empty());
    }

    #[test]
    fn per_worktree_stores_are_not_shared() {
        let policy = CachePolicy {
            node_store: ShareMode::PerWorktree,
            python_wheel_cache: ShareMode::Shared,
            cargo_registry: ShareMode::PerWorktree,
        };
        let plan = policy.plan(Path::new("/c"));
        let keys: Vec<&str> = plan.shared_env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!keys.contains(&"npm_config_cache"));
        assert!(keys.contains(&"PIP_CACHE_DIR"));
        assert_eq!(plan.per_worktree.len(), 2);
    }

    #[test]
    fn ignore_globs_cover_generated_dirs() {
        let ignores = default_watch_ignores();
        assert!(ignores.iter().any(|g| g.contains("node_modules")));
        assert!(ignores.iter().any(|g| g.contains("target")));
    }
}
