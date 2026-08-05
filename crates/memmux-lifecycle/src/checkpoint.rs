//! Checkpoint contents, storage contract, and integrity verification (§13.1 / SUM-90).
//!
//! A checkpoint is the minimal, durable description needed to *losslessly reconstruct* an idle
//! task's session — never the session's live memory. It captures:
//!
//! * the exact repository state (the `HEAD` commit plus a hash of the working-tree patch), so a
//!   resume lands on the same code the task last saw;
//! * the transcript cursor (how far into the paged history the provider had read);
//! * the provider's own session reference, if it supports native resume (e.g. a `claude`
//!   session id) — an opaque handle, not transcript contents;
//! * a resource baseline (RSS at checkpoint time) so recycling can measure reclamation;
//! * [`SecretRef`]s — *references* to secrets (env var names, file paths, keychain accounts),
//!   never the secret material itself (SUM-79).
//!
//! Every checkpoint carries an [`integrity`](Checkpoint::integrity) hash over its own contents so
//! the runtime can detect a corrupted checkpoint *before* attempting a resume. The hash is a
//! fast FNV-1a content digest — a corruption/tamper-evidence check for local artifacts, not a
//! cryptographic signature; it keeps the build self-contained (no crypto dependency), matching
//! the project's framed-JSON-over-gRPC stance.

use serde::{Deserialize, Serialize};

/// FNV-1a 64-bit content digest, rendered as zero-padded hex. Stable across runs and platforms,
/// so it is suitable for the checkpoint integrity field and for hashing git patches.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Where a referenced secret actually lives. The reference is durable and safe to persist; the
/// value is fetched only at launch time by the isolation layer (SUM-79).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretSource {
    /// An environment variable on the daemon host.
    Env {
        /// The variable name (e.g. `ANTHROPIC_API_KEY`).
        var: String,
    },
    /// A file readable by the daemon (e.g. `~/.config/....json`).
    File {
        /// Absolute path to the secret file.
        path: String,
    },
    /// An OS keychain / secret-service entry.
    Keychain {
        /// The account/entry name.
        account: String,
    },
}

/// A reference to a secret a task needs — persisted in the checkpoint *by reference only*.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    /// Logical name the provider expects (e.g. the env var it reads).
    pub name: String,
    /// Where the value is sourced from at launch time.
    pub source: SecretSource,
}

impl SecretRef {
    /// A reference that maps a provider env var to a host env var of the same name.
    pub fn env(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            source: SecretSource::Env { var: name.clone() },
            name,
        }
    }
}

/// The durable description of a checkpointed/hibernated task (§13.1).
///
/// Construct via [`Checkpoint::new`], which computes the integrity hash. The struct derives
/// `Serialize`/`Deserialize` so the daemon can store it as a JSON artifact on disk with a
/// reference row in SQLite (SUM-90 acceptance: artifacts on disk, reference in the store).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Owning task id.
    pub task_id: String,
    /// `HEAD` commit sha at checkpoint time.
    pub git_head: String,
    /// Hash of the working-tree patch (`git diff HEAD`) — pins the exact dirty state.
    pub git_patch_hash: String,
    /// How far into the paged transcript history the provider had consumed.
    pub transcript_cursor: u64,
    /// The provider's own session handle for native resume, if any.
    pub provider_session_ref: Option<String>,
    /// Resident set size (bytes) at checkpoint time — the recycling reclamation baseline.
    pub resource_baseline_bytes: u64,
    /// Secrets the task needs, referenced (never embedded).
    pub secret_refs: Vec<SecretRef>,
    /// Creation time (ms since the Unix epoch).
    pub created_at_ms: u64,
    /// Integrity digest over every other field (see [`content_hash`]).
    pub integrity: String,
}

/// Borrowed view of the integrity-covered fields, serialized deterministically to compute the
/// digest. Field order here defines the canonical hashing order.
#[derive(Serialize)]
struct IntegrityBody<'a> {
    task_id: &'a str,
    git_head: &'a str,
    git_patch_hash: &'a str,
    transcript_cursor: u64,
    provider_session_ref: &'a Option<String>,
    resource_baseline_bytes: u64,
    secret_refs: &'a [SecretRef],
    created_at_ms: u64,
}

impl Checkpoint {
    /// Build a checkpoint and compute its integrity hash.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_id: impl Into<String>,
        git_head: impl Into<String>,
        git_patch_hash: impl Into<String>,
        transcript_cursor: u64,
        provider_session_ref: Option<String>,
        resource_baseline_bytes: u64,
        secret_refs: Vec<SecretRef>,
        created_at_ms: u64,
    ) -> Self {
        let mut cp = Self {
            task_id: task_id.into(),
            git_head: git_head.into(),
            git_patch_hash: git_patch_hash.into(),
            transcript_cursor,
            provider_session_ref,
            resource_baseline_bytes,
            secret_refs,
            created_at_ms,
            integrity: String::new(),
        };
        cp.integrity = cp.compute_integrity();
        cp
    }

    fn compute_integrity(&self) -> String {
        let body = IntegrityBody {
            task_id: &self.task_id,
            git_head: &self.git_head,
            git_patch_hash: &self.git_patch_hash,
            transcript_cursor: self.transcript_cursor,
            provider_session_ref: &self.provider_session_ref,
            resource_baseline_bytes: self.resource_baseline_bytes,
            secret_refs: &self.secret_refs,
            created_at_ms: self.created_at_ms,
        };
        // Serialization of this fixed struct is deterministic, so the digest is reproducible.
        let bytes = serde_json::to_vec(&body).expect("checkpoint body serializes");
        content_hash(&bytes)
    }

    /// Whether the stored integrity hash matches the current contents (corruption check before
    /// resume — SUM-90 acceptance).
    pub fn verify(&self) -> bool {
        self.integrity == self.compute_integrity()
    }

    /// Whether the provider recorded a native session handle to resume from.
    pub fn has_native_session(&self) -> bool {
        self.provider_session_ref.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Checkpoint {
        Checkpoint::new(
            "task_1",
            "abc123",
            content_hash(b"diff --git a/x b/x\n+hello\n"),
            42,
            Some("sess_9".into()),
            512 * super::super::MIB,
            vec![SecretRef::env("ANTHROPIC_API_KEY")],
            1_000,
        )
    }

    #[test]
    fn content_hash_is_stable_and_sensitive() {
        assert_eq!(content_hash(b"hello"), content_hash(b"hello"));
        assert_ne!(content_hash(b"hello"), content_hash(b"hellp"));
        assert_eq!(content_hash(b"").len(), 16);
    }

    #[test]
    fn fresh_checkpoint_verifies() {
        assert!(sample().verify());
    }

    #[test]
    fn tampering_any_field_fails_verification() {
        let mut cp = sample();
        cp.transcript_cursor += 1; // integrity no longer matches
        assert!(!cp.verify());

        let mut cp2 = sample();
        cp2.git_patch_hash = "deadbeef".into();
        assert!(!cp2.verify());
    }

    #[test]
    fn round_trips_through_json_and_still_verifies() {
        let cp = sample();
        let json = serde_json::to_string(&cp).unwrap();
        let back: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(cp, back);
        assert!(back.verify());
    }

    #[test]
    fn secret_refs_never_carry_plaintext() {
        // The type only models references; assert the env constructor keeps name == var.
        let s = SecretRef::env("OPENAI_API_KEY");
        match &s.source {
            SecretSource::Env { var } => assert_eq!(var, "OPENAI_API_KEY"),
            other => panic!("unexpected source {other:?}"),
        }
        // A checkpoint's JSON must not contain a value field — only references.
        let json = serde_json::to_string(&sample()).unwrap();
        assert!(json.contains("ANTHROPIC_API_KEY"));
        assert!(!json.contains("\"value\""));
    }

    #[test]
    fn native_session_flag_reflects_ref() {
        assert!(sample().has_native_session());
        let mut cp = sample();
        cp.provider_session_ref = None;
        // Rebuild integrity for the mutated field so we test the flag, not verification.
        let cp = Checkpoint::new(
            cp.task_id,
            cp.git_head,
            cp.git_patch_hash,
            cp.transcript_cursor,
            None,
            cp.resource_baseline_bytes,
            cp.secret_refs,
            cp.created_at_ms,
        );
        assert!(!cp.has_native_session());
    }
}
