---
title: "Phase 2 — Lifecycle runtime"
description: "Checkpoint/hibernate, native + reconstructed resume, and RSS-threshold recycling."
---


**Goal (§13, §8.7):** freeze idle tasks to near-zero resident memory and restore them
losslessly, and recycle providers that grow too large — all without losing dirty Git state.
**Exit criterion:** reliable resume with no lost dirty state under fault tests.

Phase 2 adds the *lifecycle* on top of the Phase-1 multiplexer: a task can be checkpointed,
hibernated (its provider stopped), and later resumed — natively where the provider supports it,
or via a reconstructed session otherwise. Long-running providers that grow past an RSS threshold
are recycled: checkpointed, restarted at a safe point, and resumed, with the reclaimed memory
recorded in a ledger.

## What shipped

| Epic / story | Crate(s) | Highlights |
| --- | --- | --- |
| SUM-90 Checkpoint contents + storage | `memmux-lifecycle`, `memmux-store`, `memmuxd` | `Checkpoint` = git HEAD + working-tree patch hash + transcript cursor + provider session ref + RSS baseline + secret **references**; FNV-1a integrity verified before resume; JSON artifact on disk with a reference row in SQLite |
| SUM-91 Safe-point detection | `memmux-lifecycle` | Conservative `assess` (never mid-tool-call / mid-write) + a deadline-bounded waiter that **abandons rather than forces**; shared by hibernation and recycling |
| SUM-92 Provider-native resume | `memmux-adapters`, `memmuxd` | `ProviderAdapter::resume_command` (`claude --resume <id>`, `opencode --session <id>`); resume latency reported as an event |
| SUM-93 Resume-fidelity validation + fallback | `memmuxd` | `plan_resume` picks native / reconstructed / cold-start; post-resume validation; native-spawn failure falls back to reconstructed; total failure leaves the task hibernated with its checkpoint intact |
| SUM-94 RSS-threshold recycle trigger | `memmux-lifecycle`, `memmuxd` | Per-provider `RecyclePolicy`; throttled pump check enters recycling only at a safe point and emits a decision event with the reason + measured RSS |
| SUM-95 Shutdown + process-tree verification | `memmux-metrics`, `memmuxd` | Graceful stop then `terminate_subtree` confirms the whole subtree is gone before relaunch |
| SUM-96 Recycle validation + rollback | `memmuxd` | Resumed instance validated; on failure the task rolls back to `FAILED` with its checkpoint retained (no lost work) |
| SUM-97 Reclaimed-bytes ledger | `memmux-lifecycle`, `memmuxd`, `memmux` | `runtime_recycled` event carrying `rss_before`/`rss_after`/`reclaimed_bytes`/`resume_mode`/`resume_latency_ms`/`git_patch_hash`; surfaced inline in the TUI timeline; "no measurable reclamation" reported explicitly |
| SUM-73 Gemini CLI adapter | `memmux-adapters` | Honest capabilities (reconstructed resume) |
| SUM-74 OpenCode adapter | `memmux-adapters` | Native session resume (`--session`) |
| SUM-79 Capability-scoped adapters + secret refs | `memmux-adapters`, `memmuxd` | Adapters declare required secret **references**; `CapabilityGrant::resolve_env` resolves only the refs a task's grant allows — env/file sources, never logged, missing is non-fatal |

## Design notes

- **`memmux-lifecycle` is pure.** Like `memmux-sched`, it holds only decision logic and data
  contracts (checkpoint model, safe-point detection, recycle policy, reclamation ledger, resume
  modes), so every rule is exhaustively unit-testable without a process, socket, or clock.
- **Checkpoints reference secrets, never embed them** (SUM-79). The JSON artifact contains
  `SecretRef`s (env var names, file paths); values are resolved only at launch time.
- **Integrity is a corruption check, not a signature.** The FNV-1a digest detects a torn or
  tampered artifact before a resume; it keeps the build self-contained (no crypto dependency),
  matching the project's framed-JSON-over-gRPC stance.
- **Session-ref capture is provider-specific** and lands with deeper per-provider integration.
  Until a real session handle is captured, resume uses the reconstructed path rather than
  claiming a native resume the runtime can't yet drive — the native decision (`plan_resume`) and
  the native relaunch command are both in place and unit-tested.

## Try it

```bash
cargo build --workspace
memmuxd serve &

# Start a task, then freeze and restore it.
ID=$(memmuxd create --title demo --repo ~/src/product --provider claude-code | awk '{print $1}')
memmuxd start "$ID"
memmuxd hibernate "$ID"   # captures a checkpoint, stops the provider
memmuxd resume "$ID"      # restores it (native or reconstructed)

# Recycle a running provider (checkpoint → restart → resume → ledger).
memmuxd recycle "$ID"

# The recycle ledger shows up in the TUI timeline (view 4).
memmux
```

## Tests

- **Unit:** checkpoint integrity + secret-ref safety; safe-point verdicts + waiter abandon;
  recycle policy + reclamation summaries; resume-mode fidelity; `plan_resume` decision matrix;
  capability-scoped secret resolution (granted / denied / missing); store checkpoint-ref
  round-trip; TUI ledger rendering.
- **Integration (`crates/memmuxd/tests`):** hibernate → checkpoint artifact on disk (integrity
  present) → resume → provider live again; recycle → `runtime_recycled` ledgered → provider live.
