//! Self-update (SUM-129): `memmux update` swaps the installed binaries in place to the newest
//! release, and the TUI shows a non-blocking "update available" hint on launch.
//!
//! Kept dependency-free in the same spirit as the bootstrap installer — it shells out to `curl`
//! and `tar` rather than pulling an HTTP/TLS stack into the binary. The version-comparison and
//! archive-swap logic is pure and unit-tested; only the network fetch is not.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "sumanyumuku98/MemMux";

/// The release target triple for this build (matches the release asset names).
pub fn target() -> &'static str {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        }
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// The release archive name for a tag on this platform, e.g. `memmux-v0.3.0-aarch64-apple-darwin.tar.gz`.
pub fn asset_name(tag: &str) -> String {
    format!("memmux-{tag}-{}.tar.gz", target())
}

/// Parse a `vMAJOR.MINOR.PATCH` (or without the `v`) tag into a comparable tuple.
pub fn parse_semver(tag: &str) -> Option<(u64, u64, u64)> {
    let core = tag.trim_start_matches('v');
    // Drop any pre-release/build suffix (e.g. `-rc1`), then take the first three dotted numbers.
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut it = core.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    Some((maj, min, patch))
}

/// Whether `latest` is a strictly newer version than `current`. Unparsable versions → `false`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// The outcome of an update attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Already on the newest release.
    UpToDate(String),
    /// Upgraded from → to, replacing the named binaries.
    Updated {
        /// Version before the update.
        from: String,
        /// Version installed.
        to: String,
        /// Binaries that were replaced.
        installed: Vec<String>,
    },
}

/// Resolve the newest release tag (including pre-releases), via the GitHub API over `curl`.
pub fn newest_release_tag() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=1");
    let out = Command::new("curl")
        .args(["-fsSL", &url])
        .output()
        .context("running curl to query the GitHub API")?;
    if !out.status.success() {
        anyhow::bail!("GitHub API request failed");
    }
    let releases: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    releases
        .get(0)
        .and_then(|r| r.get("tag_name"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .context("no releases found")
}

/// If a newer release exists, return a short user-facing hint (else `None`). Network best-effort.
pub fn check_for_update(current: &str) -> Option<String> {
    let tag = newest_release_tag().ok()?;
    is_newer(&tag, current).then(|| format!("update available: {tag} — run `memmux update`"))
}

/// Update the installed binaries in `bin_dir` to the newest release (or `$MEMMUX_VERSION` if set).
/// No-ops when already current. `bin_dir` is normally the directory of the running executable.
pub fn run_update(current: &str, bin_dir: &Path) -> Result<Outcome> {
    let pinned = std::env::var("MEMMUX_VERSION")
        .ok()
        .filter(|s| !s.is_empty());
    let tag = match &pinned {
        Some(t) => t.clone(),
        None => newest_release_tag()?,
    };
    if pinned.is_none() && !is_newer(&tag, current) {
        return Ok(Outcome::UpToDate(current.to_string()));
    }

    let tmp = tempdir("memmux-update")?;
    let archive = tmp.join("release.tar.gz");
    let url = format!(
        "https://github.com/{REPO}/releases/download/{tag}/{}",
        asset_name(&tag)
    );
    let status = Command::new("curl")
        .args(["-fsSL", &url, "-o"])
        .arg(&archive)
        .status()
        .context("running curl to download the release")?;
    if !status.success() {
        anyhow::bail!("failed to download {url}");
    }

    let installed = swap_binaries(&archive, bin_dir)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(Outcome::Updated {
        from: current.to_string(),
        to: tag,
        installed,
    })
}

/// Extract `archive` (a `.tar.gz` containing `<dir>/memmux` + `<dir>/memmuxd`) and atomically
/// replace those binaries in `bin_dir`. Overwriting a running binary is safe on unix because the
/// replace is a rename over a fresh inode.
pub fn swap_binaries(archive: &Path, bin_dir: &Path) -> Result<Vec<String>> {
    let tmp = tempdir("memmux-extract")?;
    let status = Command::new("tar")
        .arg("xzf")
        .arg(archive)
        .arg("-C")
        .arg(&tmp)
        .status()
        .context("running tar to extract the release")?;
    if !status.success() {
        anyhow::bail!("failed to extract {}", archive.display());
    }

    std::fs::create_dir_all(bin_dir)?;
    let mut installed = Vec::new();
    for name in ["memmux", "memmuxd"] {
        let src = find_binary(&tmp, name)
            .with_context(|| format!("{name} not found in the release archive"))?;
        let dst = bin_dir.join(name);
        let staged = bin_dir.join(format!(".{name}.new"));
        std::fs::copy(&src, &staged)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&staged, &dst)?; // atomic replace, safe over a running binary
        installed.push(name.to_string());
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(installed)
}

/// Find `name` directly in `root` or one level down (the archive's `memmux-<tag>-<target>/` dir).
fn find_binary(root: &Path, name: &str) -> Option<PathBuf> {
    let direct = root.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if entry.path().is_dir() {
            let candidate = entry.path().join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// A unique temp directory under the system temp dir.
fn tempdir(tag: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!(
        "{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parse_and_compare() {
        assert_eq!(parse_semver("v0.3.0"), Some((0, 3, 0)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("v0.3.0-rc1"), Some((0, 3, 0)));
        assert_eq!(parse_semver("nope"), None);

        assert!(is_newer("v0.3.0", "v0.2.0"));
        assert!(is_newer("v0.2.1", "0.2.0"));
        assert!(!is_newer("v0.2.0", "v0.2.0"));
        assert!(!is_newer("v0.1.0", "v0.2.0"));
        assert!(!is_newer("garbage", "v0.2.0"));
    }

    #[test]
    fn asset_name_matches_release_naming() {
        let n = asset_name("v0.3.0");
        assert!(n.starts_with("memmux-v0.3.0-"));
        assert!(n.ends_with(".tar.gz"));
        assert!(n.contains(target()));
    }

    #[test]
    fn swap_binaries_replaces_in_place_from_an_archive() {
        // Build a fake release archive: <dir>/memmux + <dir>/memmuxd.
        let work = tempdir("memmux-swap-test").unwrap();
        let stage = work.join("memmux-v9.9.9-test");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("memmux"), b"#!/bin/sh\necho NEW memmux\n").unwrap();
        std::fs::write(stage.join("memmuxd"), b"#!/bin/sh\necho NEW memmuxd\n").unwrap();
        let archive = work.join("rel.tar.gz");
        assert!(Command::new("tar")
            .arg("czf")
            .arg(&archive)
            .arg("-C")
            .arg(&work)
            .arg("memmux-v9.9.9-test")
            .status()
            .unwrap()
            .success());

        // A bin dir with stale binaries to be replaced.
        let bin = work.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("memmux"), b"OLD").unwrap();
        std::fs::write(bin.join("memmuxd"), b"OLD").unwrap();

        let installed = swap_binaries(&archive, &bin).unwrap();
        assert_eq!(installed, vec!["memmux".to_string(), "memmuxd".to_string()]);
        assert!(std::fs::read(bin.join("memmux"))
            .unwrap()
            .starts_with(b"#!/bin/sh"));
        assert!(std::fs::read(bin.join("memmuxd"))
            .unwrap()
            .starts_with(b"#!/bin/sh"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(bin.join("memmux"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "installed binary should be executable");
        }
        std::fs::remove_dir_all(&work).ok();
    }
}
