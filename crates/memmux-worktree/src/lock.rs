//! Repository mutation lock (SUM-57 / §9.2 step 3).
//!
//! Worktree creation mutates shared repo state (refs, the worktrees list), so it must be
//! serialized. This is a cross-process advisory lock built on exclusive file creation
//! (`O_CREAT | O_EXCL`); the returned guard removes the lock file on drop.

use memmux_core::ids::RepositoryId;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// An acquirable per-repository mutation lock.
#[derive(Clone, Debug)]
pub struct RepoMutationLock {
    path: PathBuf,
}

impl RepoMutationLock {
    /// A lock for `repo` under `locks_dir`.
    pub fn new(locks_dir: impl AsRef<Path>, repo: &RepositoryId) -> Self {
        Self {
            path: locks_dir.as_ref().join(format!("{}.lock", repo.as_str())),
        }
    }

    /// Try to acquire the lock immediately, returning `None` if it is already held.
    pub fn try_acquire(&self) -> io::Result<Option<LockGuard>> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = writeln!(file, "{}", std::process::id());
                Ok(Some(LockGuard {
                    path: self.path.clone(),
                }))
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Acquire the lock, polling until `timeout` elapses.
    pub fn acquire(&self, timeout: Duration) -> io::Result<LockGuard> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(guard) = self.try_acquire()? {
                return Ok(guard);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("timed out acquiring repo lock {}", self.path.display()),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Holds the repository mutation lock; releases it on drop.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    /// The lock file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("memmux-lock-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn exclusive_then_released_on_drop() {
        let dir = tmp("excl");
        let repo = RepositoryId::new("repo_1");
        let lock = RepoMutationLock::new(&dir, &repo);

        let g = lock.try_acquire().unwrap().expect("first acquire");
        // Second acquire while held fails fast.
        assert!(lock.try_acquire().unwrap().is_none());
        assert!(lock.acquire(Duration::from_millis(30)).is_err());

        drop(g);
        // After release, it can be acquired again.
        assert!(lock.try_acquire().unwrap().is_some());

        fs::remove_dir_all(&dir).ok();
    }
}
