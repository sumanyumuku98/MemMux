//! # memmux-worktree
//!
//! Git worktree orchestration (§9). Implemented over the `git` CLI — the spec names
//! git-worktree documentation as the source of truth (Appendix D), and using real git keeps
//! worktree/branch/merge semantics faithful without a native-library build.
//!
//! * [`slug`] — collision-resistant task slugs and branch names (SUM-58).
//! * [`layout`] — the managed `~/.memmux` directory layout.
//! * [`lock`] — repository mutation lock serializing worktree creation (SUM-57).
//! * [`conflict`] — overlapping changed-file detection and conflict-risk scoring (SUM-61).
//! * [`cache`] — dependency/cache sharing strategy (SUM-60).
//! * [`worktree`] — the manager: create, dirty-state protection, completion flows
//!   (SUM-57, SUM-59, SUM-62).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cache;
pub mod conflict;
pub mod gitcmd;
pub mod layout;
pub mod lock;
pub mod slug;
pub mod worktree;

pub use cache::{CachePlan, CachePolicy, ShareMode};
pub use conflict::{conflict_risk, detect_conflicts, ChangedFiles, ConflictReport, Overlap};
pub use layout::ManagedLayout;
pub use lock::{LockGuard, RepoMutationLock};
pub use slug::TaskSlug;
pub use worktree::{Completion, CompletionResult, DirtyManifest, WorktreeHandle, WorktreeManager};
