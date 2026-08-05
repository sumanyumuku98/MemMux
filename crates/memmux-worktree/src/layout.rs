//! The managed `~/.memmux` directory layout (§15.3).
//!
//! Task manifests live *outside* the worktree (so removing a worktree never loses task
//! metadata); worktrees, locks, and per-task artifacts each have a stable home.

use memmux_core::ids::RepositoryId;
use std::path::{Path, PathBuf};

/// Computes paths under a managed MemMux root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedLayout {
    root: PathBuf,
}

impl ManagedLayout {
    /// Use an explicit root (tests point this at a temp dir).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default root: `$HOME/.memmux` (falls back to `./.memmux`).
    pub fn default_root() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            root: home.join(".memmux"),
        }
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding all worktrees for a repository.
    pub fn worktrees_dir(&self, repo: &RepositoryId) -> PathBuf {
        self.root.join("worktrees").join(repo.as_str())
    }

    /// The worktree path for a given task slug.
    pub fn worktree_path(&self, repo: &RepositoryId, slug: &str) -> PathBuf {
        self.worktrees_dir(repo).join(slug)
    }

    /// The task manifest path (outside the worktree).
    pub fn manifest_path(&self, task_id: &str) -> PathBuf {
        self.root.join("tasks").join(task_id).join("manifest.json")
    }

    /// The directory for repository mutation lock files.
    pub fn locks_dir(&self) -> PathBuf {
        self.root.join("locks")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_namespaced_by_repo_and_slug() {
        let layout = ManagedLayout::new("/tmp/mm");
        let repo = RepositoryId::new("repo_abc");
        assert_eq!(
            layout.worktrees_dir(&repo),
            PathBuf::from("/tmp/mm/worktrees/repo_abc")
        );
        assert_eq!(
            layout.worktree_path(&repo, "fix-bug-abc1234"),
            PathBuf::from("/tmp/mm/worktrees/repo_abc/fix-bug-abc1234")
        );
        assert_eq!(layout.locks_dir(), PathBuf::from("/tmp/mm/locks"));
    }

    #[test]
    fn manifest_lives_outside_worktrees() {
        let layout = ManagedLayout::new("/tmp/mm");
        let manifest = layout.manifest_path("task_1");
        assert!(manifest.starts_with("/tmp/mm/tasks"));
        assert!(!manifest.to_string_lossy().contains("/worktrees/"));
    }
}
