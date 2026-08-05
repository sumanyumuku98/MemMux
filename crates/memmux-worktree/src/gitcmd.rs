//! Thin wrapper around the `git` CLI.
//!
//! Using real git keeps worktree/branch/merge semantics faithful (Appendix D). Every call is a
//! single `git -C <repo> <args…>` invocation with captured output.

use std::path::Path;
use std::process::Command;

/// The captured result of a git invocation.
#[derive(Clone, Debug)]
pub struct GitOutput {
    /// Whether git exited 0.
    pub success: bool,
    /// Captured stdout (trimmed of the trailing newline).
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// Run `git -C <repo> <args…>` and capture its output.
pub fn git(repo: &Path, args: &[&str]) -> anyhow::Result<GitOutput> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git {:?}: {e}", args))?;
    Ok(GitOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout)
            .trim_end_matches('\n')
            .to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Run git and error if it exits non-zero, returning trimmed stdout on success.
pub fn git_ok(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = git(repo, args)?;
    if out.success {
        Ok(out.stdout)
    } else {
        Err(anyhow::anyhow!(
            "git {:?} failed: {}",
            args,
            out.stderr.trim()
        ))
    }
}
