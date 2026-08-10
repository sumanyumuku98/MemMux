//! Bounded-footprint-under-overcommit (H4a / SUM-167) and no-lost-work (H4b / SUM-168) measurement.
//!
//! H4 is a *governance* claim. Under a **constrained agent budget** — one set below the aggregate
//! predicted peak of N concurrently-live agents — an ungoverned multiplexer (raw / tmux / herdr)
//! simply runs all N agents, so its footprint grows to ≈ N × per-agent. MemMux instead **governs**:
//! its admission planner defers starts that would not fit the budget, and its pressure ladder
//! reclaims resident agents (hibernate / recycle / — at the Emergency stage — preserve-then-
//! terminate) so the aggregate footprint stays near/under budget.
//!
//! Two honest, event-sourced metrics come out of one overcommit run, read from MemMux's own audit /
//! event stream over the `ReadEvents` API (never fabricated for a competitor):
//!
//! * **H4a — bounded footprint.** Per launcher we already sample peak/steady *total footprint*
//!   (see [`crate::sampler`]); here we additionally count MemMux's **governance actions**
//!   (admission deferrals + pressure-ladder reclamations). Baselines have no such mechanism, so
//!   their governance-action count is *unsupported* ([`None`]), rendered exactly like H3 —
//!   never a fabricated number.
//! * **H4b — no lost work.** When the ladder must terminate a victim under pressure it first
//!   captures a durable checkpoint (git HEAD + patch hash + dirty manifest). We count
//!   `victims_reclaimed` (pressure terminations) and `checkpoints_captured` (preserve events whose
//!   payload carries a **non-empty** git patch hash), and report the preserved fraction. Baselines
//!   have no checkpoint mechanism at all, so this is *unsupported* ([`None`]) — the honest claim is
//!   a **capability** comparison, NOT "baselines lost N files".

use memmux_proto::EventView;
use serde::{Deserialize, Serialize};

/// The event category the daemon emits admission-deferral events under.
const CAT_ADMISSION: &str = "admission";
/// The event category the daemon emits pressure-ladder actions under.
const CAT_PRESSURE: &str = "pressure";
/// Admission-deferral event type (a start that did not fit the budget was held).
const EV_DEFERRED: &str = "deferred";
/// The pressure-ladder reclamation event types (each is an actual reclamation action).
const RECLAIM_EVENT_TYPES: [&str; 5] = [
    "reclaim_idle_children",
    "hibernate_low_priority",
    "recycle_bloated",
    "preserve_git_state",
    "terminate_lowest_priority",
];
/// The pressure-ladder terminal event: a victim was terminated to reclaim its footprint. This is
/// the H4b "victim reclaimed" denominator.
const EV_TERMINATE: &str = "terminate_lowest_priority";
/// The pressure-ladder event that captures a victim's checkpoint before termination. Its payload
/// carries the git HEAD + patch hash + `git_dirty` flag (the H4b numerator when `git_dirty`).
const EV_PRESERVE: &str = "preserve_git_state";

/// Bounded-footprint-under-overcommit governance evidence for one launcher (SUM-167 / H4a).
///
/// The footprint numbers themselves come from the sampled [`crate::report::RunSummary`]; this
/// struct carries only the **governance-action** counts, which are `Some` for MemMux and `None`
/// (unsupported — no governance mechanism) for the baselines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OvercommitGovernance {
    /// Admission starts deferred because they did not fit the constrained budget.
    pub admission_deferrals: usize,
    /// Pressure-ladder reclamation actions executed (idle-child reclaim, hibernate, recycle,
    /// preserve, terminate).
    pub reclamations: usize,
}

impl OvercommitGovernance {
    /// Total governance actions (deferrals + reclamations) — the headline H4a evidence count.
    pub fn total_actions(&self) -> usize {
        self.admission_deferrals + self.reclamations
    }
}

/// No-lost-work evidence for one launcher (SUM-168 / H4b).
///
/// `victims_reclaimed` is the number of pressure-driven terminations; `checkpoints_captured` is the
/// number of those victims for which a checkpoint carrying a **non-empty** git patch hash was
/// captured first. `preserved_fraction` is `captured / reclaimed` (`None` when nothing was
/// reclaimed — the metric is n/a, never a fabricated 100%).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NoLostWork {
    /// Pressure-driven victim terminations observed this run.
    pub victims_reclaimed: usize,
    /// Victims for which a checkpoint with a non-empty git patch hash was captured before
    /// termination.
    pub checkpoints_captured: usize,
}

impl NoLostWork {
    /// Fraction of reclaimed victims whose work was preserved (`captured / reclaimed`), or `None`
    /// when no victim was reclaimed (the measurement is n/a).
    pub fn preserved_fraction(&self) -> Option<f64> {
        if self.victims_reclaimed == 0 {
            None
        } else {
            Some(self.checkpoints_captured as f64 / self.victims_reclaimed as f64)
        }
    }
}

/// A launcher's combined H4 evidence for one overcommit run: the H4a governance counts and the H4b
/// no-lost-work counts. Only a governed launcher (MemMux) produces this; baselines return `None`
/// (unsupported / ungoverned).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct H4Evidence {
    /// H4a governance-action counts (admission deferrals + pressure reclamations).
    pub governance: OvercommitGovernance,
    /// H4b no-lost-work counts (victims reclaimed vs checkpoints captured).
    pub no_lost_work: NoLostWork,
}

/// Classify a MemMux event stream into combined [`H4Evidence`] (SUM-167/168).
pub fn evidence_from_events(events: &[EventView]) -> H4Evidence {
    let (governance, no_lost_work) = classify_events(events);
    H4Evidence {
        governance,
        no_lost_work,
    }
}

/// Classify a MemMux event stream into the H4a governance counts and the H4b no-lost-work counts
/// (SUM-167/168). Pure over `events` so it is unit-testable without a live daemon.
///
/// A `preserve_git_state` event counts toward `checkpoints_captured` only when its payload's
/// `git_dirty` flag is `true` — i.e. a real uncommitted-work patch was captured, not the empty-diff
/// sentinel. Returns `(governance, no_lost_work)`.
pub fn classify_events(events: &[EventView]) -> (OvercommitGovernance, NoLostWork) {
    let mut gov = OvercommitGovernance::default();
    let mut nlw = NoLostWork::default();
    for ev in events {
        if ev.category == CAT_ADMISSION && ev.event_type == EV_DEFERRED {
            gov.admission_deferrals += 1;
        }
        if ev.category == CAT_PRESSURE {
            if RECLAIM_EVENT_TYPES.contains(&ev.event_type.as_str()) {
                gov.reclamations += 1;
            }
            if ev.event_type == EV_TERMINATE {
                nlw.victims_reclaimed += 1;
            }
            if ev.event_type == EV_PRESERVE && payload_git_dirty(ev.payload_json.as_deref()) {
                nlw.checkpoints_captured += 1;
            }
        }
    }
    (gov, nlw)
}

/// Whether a `preserve_git_state` payload reports a non-empty git patch (`"git_dirty": true`).
/// Dependency-free scan mirroring the launcher's JSON helpers (no serde_json dependency here).
fn payload_git_dirty(payload: Option<&str>) -> bool {
    let Some(p) = payload else { return false };
    // Find `"git_dirty"`, skip to the value, and check for a literal `true`.
    let Some(idx) = p.find("\"git_dirty\"") else {
        return false;
    };
    let rest = &p[idx + "\"git_dirty\"".len()..];
    // Skip the colon and any whitespace, then test the token.
    let rest = rest.trim_start_matches([' ', '\t', ':']);
    rest.starts_with("true")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(category: &str, event_type: &str, payload: Option<&str>) -> EventView {
        EventView {
            seq: 0,
            task_id: None,
            ts_ms: 0,
            category: category.to_string(),
            event_type: event_type.to_string(),
            severity: "info".to_string(),
            source: "daemon".to_string(),
            payload_json: payload.map(str::to_string),
        }
    }

    #[test]
    fn counts_deferrals_and_reclamations() {
        let events = vec![
            ev(CAT_ADMISSION, EV_DEFERRED, None),
            ev(CAT_ADMISSION, EV_DEFERRED, None),
            ev(CAT_PRESSURE, "hibernate_low_priority", None),
            ev(CAT_PRESSURE, "recycle_bloated", None),
            // Unrelated events are ignored.
            ev("lifecycle", "started", None),
        ];
        let (gov, nlw) = classify_events(&events);
        assert_eq!(gov.admission_deferrals, 2);
        assert_eq!(gov.reclamations, 2);
        assert_eq!(gov.total_actions(), 4);
        // No terminations this run → no-lost-work is n/a.
        assert_eq!(nlw.victims_reclaimed, 0);
        assert_eq!(nlw.preserved_fraction(), None);
    }

    #[test]
    fn preserve_then_terminate_is_no_lost_work() {
        let dirty = r#"{"git_head":"abc","git_patch_hash":"deadbeef","git_dirty":true}"#;
        let events = vec![
            ev(CAT_PRESSURE, EV_PRESERVE, Some(dirty)),
            ev(CAT_PRESSURE, EV_TERMINATE, None),
        ];
        let (gov, nlw) = classify_events(&events);
        // preserve + terminate are both reclamation actions.
        assert_eq!(gov.reclamations, 2);
        assert_eq!(nlw.victims_reclaimed, 1);
        assert_eq!(nlw.checkpoints_captured, 1);
        assert_eq!(nlw.preserved_fraction(), Some(1.0));
    }

    #[test]
    fn preserve_with_empty_patch_is_not_counted_as_captured() {
        // A clean repo yields the empty-diff sentinel → git_dirty:false → not a captured patch.
        let clean = r#"{"git_patch_hash":"cbf29ce484222325","git_dirty":false}"#;
        let events = vec![
            ev(CAT_PRESSURE, EV_PRESERVE, Some(clean)),
            ev(CAT_PRESSURE, EV_TERMINATE, None),
        ];
        let (_gov, nlw) = classify_events(&events);
        assert_eq!(nlw.victims_reclaimed, 1);
        assert_eq!(nlw.checkpoints_captured, 0);
        assert_eq!(nlw.preserved_fraction(), Some(0.0));
    }

    #[test]
    fn payload_git_dirty_tolerates_spacing() {
        assert!(payload_git_dirty(Some(r#"{"git_dirty": true}"#)));
        assert!(payload_git_dirty(Some(r#"{"git_dirty":true}"#)));
        assert!(!payload_git_dirty(Some(r#"{"git_dirty":false}"#)));
        assert!(!payload_git_dirty(Some(r#"{"other":true}"#)));
        assert!(!payload_git_dirty(None));
    }

    #[test]
    fn empty_stream_is_all_zero_and_na() {
        let (gov, nlw) = classify_events(&[]);
        assert_eq!(gov.total_actions(), 0);
        assert_eq!(nlw.preserved_fraction(), None);
    }
}
