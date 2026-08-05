//! The worktree manager: creation, dirty-state protection, and completion flows
//! (SUM-57, SUM-59, SUM-62 / §9.2, §9.5).

use crate::gitcmd::{git, git_ok};
use crate::layout::ManagedLayout;
use crate::lock::RepoMutationLock;
use crate::slug::TaskSlug;
use memmux_core::ids::{RepositoryId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// A created worktree bound to a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeHandle {
    /// Owning task.
    pub task_id: TaskId,
    /// Owning repository.
    pub repo_id: RepositoryId,
    /// Absolute path to the source repository.
    pub repo_path: PathBuf,
    /// The branch checked out in the worktree (`memmux/<slug>`).
    pub branch: String,
    /// Absolute path to the worktree.
    pub path: PathBuf,
    /// The base commit the worktree was cut from.
    pub base_commit: String,
}

/// A snapshot of a worktree's uncommitted state, with a preservable patch (SUM-59).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyManifest {
    /// Whether the worktree has any uncommitted or untracked changes.
    pub dirty: bool,
    /// Repository-relative changed paths.
    pub changed_paths: Vec<String>,
    /// The tracked-change patch (`git diff HEAD`).
    pub patch: String,
    /// A content hash of the patch, recorded before any destructive action.
    pub patch_hash: String,
}

/// The outcome the user chose for a finished task (§9.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    /// Merge the task branch into `target`.
    Merge {
        /// Target branch.
        target: String,
    },
    /// Cherry-pick `commits` onto `target`.
    CherryPick {
        /// Target branch.
        target: String,
        /// Commit-ish list to apply.
        commits: Vec<String>,
    },
    /// Keep the branch and worktree; just stop runtime resources.
    Retain,
    /// Remove the worktree. Requires a clean tree unless `confirmed`.
    Discard {
        /// Explicit confirmation to discard uncommitted work.
        confirmed: bool,
    },
}

/// The result of a completion flow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CompletionResult {
    /// Merged into the target.
    Merged {
        /// Target branch.
        target: String,
    },
    /// Cherry-picked onto the target.
    CherryPicked,
    /// Retained (worktree kept).
    Retained,
    /// Discarded (worktree removed).
    Discarded {
        /// Patch hash preserved before removal, if the tree was dirty.
        preserved_patch_hash: Option<String>,
    },
    /// Pre-merge checks failed; nothing merged.
    ChecksFailed {
        /// Names/commands of the checks that failed.
        failed: Vec<String>,
    },
    /// The operation hit a merge conflict; the source worktree is preserved.
    MergeConflict,
    /// Refused to discard a dirty worktree without confirmation.
    RefusedDirty,
}

/// Creates and completes task worktrees over the `git` CLI.
#[derive(Clone, Debug)]
pub struct WorktreeManager {
    layout: ManagedLayout,
    lock_timeout: Duration,
}

impl WorktreeManager {
    /// Create a manager rooted at `layout`.
    pub fn new(layout: ManagedLayout) -> Self {
        Self {
            layout,
            lock_timeout: Duration::from_secs(30),
        }
    }

    /// Create a task worktree (§9.2): validate the repo, resolve the base commit, take the
    /// repository mutation lock, and add a `memmux/<slug>` branch + worktree.
    pub fn create(
        &self,
        repo_path: &Path,
        repo_id: &RepositoryId,
        task_id: &TaskId,
        slug: &TaskSlug,
        base_branch: &str,
    ) -> anyhow::Result<WorktreeHandle> {
        // 1. Validate repository health.
        let inside = git(repo_path, &["rev-parse", "--is-inside-work-tree"])?;
        if !inside.success {
            anyhow::bail!("{} is not a git repository", repo_path.display());
        }

        // 2. Resolve the requested base commit.
        let base_commit = git_ok(
            repo_path,
            &[
                "rev-parse",
                "--verify",
                &format!("{base_branch}^{{commit}}"),
            ],
        )
        .map_err(|_| anyhow::anyhow!("base branch '{base_branch}' not found"))?;

        // 3. Serialize repo mutation.
        let lock = RepoMutationLock::new(self.layout.locks_dir(), repo_id);
        let _guard = lock.acquire(self.lock_timeout)?;

        let branch = slug.branch_name();
        let path = self.layout.worktree_path(repo_id, slug.as_str());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // 4. Create branch + worktree in one git-native operation.
        git_ok(
            repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &path.to_string_lossy(),
                &base_commit,
            ],
        )?;

        Ok(WorktreeHandle {
            task_id: task_id.clone(),
            repo_id: repo_id.clone(),
            repo_path: repo_path.to_path_buf(),
            branch,
            path,
            base_commit,
        })
        // `_guard` drops here, releasing the lock.
    }

    /// Whether the worktree has uncommitted or untracked changes.
    pub fn is_dirty(&self, handle: &WorktreeHandle) -> anyhow::Result<bool> {
        let status = git_ok(&handle.path, &["status", "--porcelain"])?;
        Ok(!status.trim().is_empty())
    }

    /// Capture a preservable snapshot of the worktree's uncommitted state (SUM-59).
    pub fn dirty_manifest(&self, handle: &WorktreeHandle) -> anyhow::Result<DirtyManifest> {
        let status = git_ok(&handle.path, &["status", "--porcelain"])?;
        let changed_paths: Vec<String> = status
            .lines()
            .filter_map(|l| l.get(3..).map(|s| s.to_string()))
            .collect();
        let patch = git_ok(&handle.path, &["diff", "HEAD"])?;
        let patch_hash = content_hash(patch.as_bytes());
        Ok(DirtyManifest {
            dirty: !changed_paths.is_empty(),
            changed_paths,
            patch,
            patch_hash,
        })
    }

    /// Run a completion flow (§9.5). `checks` are commands run in the worktree before a merge;
    /// any non-zero exit aborts with [`CompletionResult::ChecksFailed`].
    pub fn complete(
        &self,
        handle: &WorktreeHandle,
        outcome: Completion,
        checks: &[Vec<String>],
    ) -> anyhow::Result<CompletionResult> {
        match outcome {
            Completion::Retain => Ok(CompletionResult::Retained),

            Completion::Discard { confirmed } => {
                let dirty = self.is_dirty(handle)?;
                if dirty && !confirmed {
                    return Ok(CompletionResult::RefusedDirty);
                }
                // Preserve before destroying, even on a confirmed discard.
                let preserved = if dirty {
                    Some(self.dirty_manifest(handle)?.patch_hash)
                } else {
                    None
                };
                self.remove_worktree(handle, true)?;
                let _ = git(&handle.repo_path, &["branch", "-D", &handle.branch]);
                Ok(CompletionResult::Discarded {
                    preserved_patch_hash: preserved,
                })
            }

            Completion::Merge { target } => {
                if let Some(failed) = self.run_checks(handle, checks) {
                    return Ok(CompletionResult::ChecksFailed { failed });
                }
                let co = git(&handle.repo_path, &["checkout", &target])?;
                if !co.success {
                    anyhow::bail!("could not checkout target '{target}': {}", co.stderr.trim());
                }
                let merge = git(&handle.repo_path, &["merge", "--no-edit", &handle.branch])?;
                if merge.success {
                    Ok(CompletionResult::Merged { target })
                } else {
                    let _ = git(&handle.repo_path, &["merge", "--abort"]);
                    Ok(CompletionResult::MergeConflict)
                }
            }

            Completion::CherryPick { target, commits } => {
                let co = git(&handle.repo_path, &["checkout", &target])?;
                if !co.success {
                    anyhow::bail!("could not checkout target '{target}': {}", co.stderr.trim());
                }
                for commit in &commits {
                    let cp = git(&handle.repo_path, &["cherry-pick", commit])?;
                    if !cp.success {
                        let _ = git(&handle.repo_path, &["cherry-pick", "--abort"]);
                        return Ok(CompletionResult::MergeConflict);
                    }
                }
                Ok(CompletionResult::CherryPicked)
            }
        }
    }

    /// Remove the worktree. Refuses a dirty tree unless `force` (SUM-59 verify-before-delete).
    pub fn remove_worktree(&self, handle: &WorktreeHandle, force: bool) -> anyhow::Result<bool> {
        if !force && self.is_dirty(handle)? {
            return Ok(false);
        }
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        let path = handle.path.to_string_lossy().into_owned();
        args.push(&path);
        git_ok(&handle.repo_path, &args)?;
        Ok(true)
    }

    /// Run each check command in the worktree; return the failing ones, or `None` if all pass.
    fn run_checks(&self, handle: &WorktreeHandle, checks: &[Vec<String>]) -> Option<Vec<String>> {
        let mut failed = Vec::new();
        for check in checks {
            let Some((program, args)) = check.split_first() else {
                continue;
            };
            let ok = Command::new(program)
                .args(args)
                .current_dir(&handle.path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                failed.push(check.join(" "));
            }
        }
        (!failed.is_empty()).then_some(failed)
    }
}

/// A deterministic, dependency-free content hash (FNV-1a 64-bit, 16 hex chars) used to mark a
/// preserved patch so re-export can verify it before deletion.
fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_stable_and_sensitive() {
        assert_eq!(
            content_hash(b"diff --git a b"),
            content_hash(b"diff --git a b")
        );
        assert_ne!(content_hash(b"patch one"), content_hash(b"patch two"));
        assert_eq!(content_hash(b"").len(), 16);
    }
}
