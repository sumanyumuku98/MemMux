//! Collision-resistant task slugs and branch names (SUM-58 / §9.2).
//!
//! A slug is derived from the task's intent plus a short deterministic hash of
//! `intent + uniqueness` (e.g. the task id), so two concurrent tasks with the same intent still
//! get distinct, filesystem- and git-ref-safe names.

use serde::{Deserialize, Serialize};

const MAX_INTENT_LEN: usize = 24;
const BRANCH_PREFIX: &str = "memmux";

/// A validated task slug and its derived branch name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSlug {
    slug: String,
}

impl TaskSlug {
    /// Generate a slug from a free-text `intent` and a `uniqueness` seed (e.g. the task id).
    pub fn generate(intent: &str, uniqueness: &str) -> Self {
        let base = {
            let s = slugify(intent);
            if s.is_empty() {
                "task".to_string()
            } else {
                truncate_on_boundary(&s, MAX_INTENT_LEN)
            }
        };
        let suffix = short_hash(&format!("{intent}\u{0}{uniqueness}"));
        Self {
            slug: format!("{base}-{suffix}"),
        }
    }

    /// The slug string (filesystem- and ref-safe).
    pub fn as_str(&self) -> &str {
        &self.slug
    }

    /// The git branch name for this slug: `memmux/<slug>`.
    pub fn branch_name(&self) -> String {
        format!("{BRANCH_PREFIX}/{}", self.slug)
    }

    /// Whether the branch name is a valid git ref (see `git check-ref-format` rules).
    pub fn is_valid_git_ref(&self) -> bool {
        is_valid_ref(&self.branch_name())
    }
}

/// Lowercase, replace non-`[a-z0-9]` with `-`, collapse and trim dashes.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn truncate_on_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_matches('-').to_string()
}

/// FNV-1a 64-bit hash rendered as 7 base-36 chars (deterministic, dependency-free).
fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let mut b36 = base36(h);
    // Left-pad / trim to a stable 7-char width.
    while b36.len() < 7 {
        b36.insert(0, '0');
    }
    b36[b36.len() - 7..].to_string()
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).expect("base36 digits are ASCII")
}

/// A conservative subset of `git check-ref-format` validation.
fn is_valid_ref(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.ends_with('/') || name.ends_with(".lock") {
        return false;
    }
    if name.contains("..") || name.contains("//") || name.contains("@{") {
        return false;
    }
    for component in name.split('/') {
        if component.is_empty() || component.starts_with('.') || component.ends_with('.') {
            return false;
        }
    }
    name.chars().all(|c| {
        !c.is_control() && !matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '\x7f')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_kebab_and_bounded() {
        let s = TaskSlug::generate("Refactor the Authentication System!!!", "task_1");
        // Human-readable prefix, truncated to the 24-char intent cap.
        assert!(
            s.as_str().starts_with("refactor-the-auth"),
            "slug was {}",
            s.as_str()
        );
        assert!(s.is_valid_git_ref());
        assert!(s.branch_name().starts_with("memmux/"));
    }

    #[test]
    fn same_inputs_are_deterministic() {
        let a = TaskSlug::generate("build feature", "task_42");
        let b = TaskSlug::generate("build feature", "task_42");
        assert_eq!(a, b);
    }

    #[test]
    fn same_intent_different_task_avoids_collision() {
        let a = TaskSlug::generate("fix bug", "task_1");
        let b = TaskSlug::generate("fix bug", "task_2");
        assert_ne!(a.as_str(), b.as_str());
        // Shared human-readable prefix, distinct hash suffix.
        assert!(a.as_str().starts_with("fix-bug-"));
        assert!(b.as_str().starts_with("fix-bug-"));
    }

    #[test]
    fn empty_or_symbolic_intent_still_valid() {
        let s = TaskSlug::generate("!!!", "task_x");
        assert!(s.as_str().starts_with("task-"));
        assert!(s.is_valid_git_ref());
    }

    #[test]
    fn rejects_unsafe_refs() {
        assert!(!is_valid_ref("memmux/bad..name"));
        assert!(!is_valid_ref("memmux/has space"));
        assert!(!is_valid_ref("memmux/trailing.lock"));
        assert!(!is_valid_ref("memmux/.hidden"));
        assert!(is_valid_ref("memmux/good-name-abc123"));
    }

    #[test]
    fn hash_suffix_is_seven_chars() {
        let s = TaskSlug::generate("x", "y");
        let suffix = s.as_str().rsplit('-').next().unwrap();
        assert_eq!(suffix.len(), 7);
    }
}
