//! Integration tests exercising the worktree manager against a real git repository.

use memmux_core::ids::{RepositoryId, TaskId};
use memmux_worktree::worktree::{Completion, CompletionResult};
use memmux_worktree::{ManagedLayout, TaskSlug, WorktreeManager};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// Create a fresh repo with one commit on `main`.
fn init_repo(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("memmux-wt-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let repo = base.join("repo");
    fs::create_dir_all(&repo).unwrap();
    run(&repo, &["init", "-b", "main"]);
    run(&repo, &["config", "user.email", "t@example.com"]);
    run(&repo, &["config", "user.name", "Test"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    run(&repo, &["add", "."]);
    run(&repo, &["commit", "-m", "initial"]);
    (base, repo)
}

fn manager(base: &Path) -> WorktreeManager {
    WorktreeManager::new(ManagedLayout::new(base.join("mm")))
}

#[test]
fn create_detect_dirty_and_merge() {
    let (base, repo) = init_repo("merge");
    let mgr = manager(&base);
    let repo_id = RepositoryId::new("repo_1");
    let task_id = TaskId::new("task_1");
    let slug = TaskSlug::generate("add greeting", "task_1");

    let handle = mgr
        .create(&repo, &repo_id, &task_id, &slug, "main")
        .expect("create worktree");
    assert!(handle.path.exists());
    assert_eq!(handle.branch, slug.branch_name());
    assert!(!mgr.is_dirty(&handle).unwrap(), "fresh worktree is clean");

    // Make a change in the worktree.
    fs::write(handle.path.join("greeting.txt"), "bonjour\n").unwrap();
    assert!(mgr.is_dirty(&handle).unwrap());
    let manifest = mgr.dirty_manifest(&handle).unwrap();
    assert!(manifest
        .changed_paths
        .iter()
        .any(|p| p.contains("greeting.txt")));

    // Commit it, then merge the branch into main via a passing check.
    run(&handle.path, &["add", "."]);
    run(&handle.path, &["commit", "-m", "add greeting"]);
    let result = mgr
        .complete(
            &handle,
            Completion::Merge {
                target: "main".into(),
            },
            &[vec!["true".into()]],
        )
        .expect("merge");
    assert_eq!(
        result,
        CompletionResult::Merged {
            target: "main".into()
        }
    );
    // The merged file now exists in the main working tree.
    assert!(repo.join("greeting.txt").exists());

    fs::remove_dir_all(&base).ok();
}

#[test]
fn failing_check_blocks_merge() {
    let (base, repo) = init_repo("checks");
    let mgr = manager(&base);
    let handle = mgr
        .create(
            &repo,
            &RepositoryId::new("repo_1"),
            &TaskId::new("task_2"),
            &TaskSlug::generate("x", "task_2"),
            "main",
        )
        .unwrap();
    run(&handle.path, &["commit", "--allow-empty", "-m", "noop"]);
    let result = mgr
        .complete(
            &handle,
            Completion::Merge {
                target: "main".into(),
            },
            &[vec!["false".into()]],
        )
        .unwrap();
    match result {
        CompletionResult::ChecksFailed { failed } => assert_eq!(failed, vec!["false".to_string()]),
        other => panic!("expected ChecksFailed, got {other:?}"),
    }
    fs::remove_dir_all(&base).ok();
}

#[test]
fn discard_refuses_dirty_without_confirmation() {
    let (base, repo) = init_repo("discard");
    let mgr = manager(&base);
    let handle = mgr
        .create(
            &repo,
            &RepositoryId::new("repo_1"),
            &TaskId::new("task_3"),
            &TaskSlug::generate("y", "task_3"),
            "main",
        )
        .unwrap();
    fs::write(handle.path.join("wip.txt"), "unsaved work\n").unwrap();

    // Unconfirmed discard of a dirty tree is refused.
    let refused = mgr
        .complete(&handle, Completion::Discard { confirmed: false }, &[])
        .unwrap();
    assert_eq!(refused, CompletionResult::RefusedDirty);
    assert!(
        handle.path.exists(),
        "worktree must survive a refused discard"
    );

    // Confirmed discard preserves a patch hash and removes the worktree.
    let discarded = mgr
        .complete(&handle, Completion::Discard { confirmed: true }, &[])
        .unwrap();
    match discarded {
        CompletionResult::Discarded {
            preserved_patch_hash,
        } => {
            assert!(preserved_patch_hash.is_some());
        }
        other => panic!("expected Discarded, got {other:?}"),
    }
    assert!(
        !handle.path.exists(),
        "worktree should be removed after confirmed discard"
    );

    fs::remove_dir_all(&base).ok();
}

#[test]
fn create_fails_for_missing_base_branch() {
    let (base, repo) = init_repo("nobase");
    let mgr = manager(&base);
    let err = mgr
        .create(
            &repo,
            &RepositoryId::new("repo_1"),
            &TaskId::new("task_4"),
            &TaskSlug::generate("z", "task_4"),
            "nonexistent",
        )
        .unwrap_err();
    assert!(err.to_string().contains("base branch"));
    fs::remove_dir_all(&base).ok();
}
