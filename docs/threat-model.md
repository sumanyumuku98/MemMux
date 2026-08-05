# MemMux threat model & security checklist

This documents the process-ownership, isolation, and security posture of MemMux (§14 of the
V2 specification, SUM-20). It is a living document: each row notes the current state of the
mitigation and where it is implemented.

## Assets

- **Developer work** — uncommitted Git changes in task worktrees (highest value; never lose).
- **Host stability** — the workstation must not be driven into destructive swap.
- **Secrets** — API keys / tokens the providers use.
- **Task metadata & audit trail** — the durable record of what MemMux did and why.

## Trust boundaries

- **Clients ↔ daemon**: local only, over a Unix domain socket owned by the user.
- **Daemon ↔ provider processes**: providers are *untrusted, supervised workloads*, each in an
  owned process group / worktree.
- **Daemon ↔ adapters**: first-party adapters run in-process; third-party adapters must run
  out of process under a restricted plugin runtime (Phase 5 SDK).

## Risk / mitigation matrix (§14.3)

| Risk | Mitigation | Status |
| --- | --- | --- |
| Agent escapes ownership | Periodic reconciliation sweep classifies every process owned/shared/escaped/unknown and surfaces escapes; recursive subtree termination reaps owned trees | ✅ `memmux-metrics::{reconcile, terminate_subtree}` (SUM-76/77) |
| Automatic termination loses work | Dirty-state protection: persist patch + hash and refuse to delete a dirty worktree without confirmation; the pressure ladder's emergency stage preserves Git state before any termination | ✅ `memmux-worktree` (SUM-59), `memmux-sched::pressure` (SUM-48) |
| Unknown/orphaned processes hidden | Reconciliation reports unknown processes with name + bytes; never silently dropped | ✅ `memmux-metrics::sweep` (SUM-77) |
| Local socket abused by other users | Socket is `0600` under a `0700` directory; user-only access | ✅ `memmuxd::server` (SUM-63/78), asserted in tests |
| Secrets copied into checkpoints/events | Store secret *references* only; event payloads carry references, never plaintext; support bundle explicitly excludes secret material | ✅ `memmux-store`, `memmuxd support-bundle` (SUM-67/68) |
| Malicious terminal output | Bounded capture with byte/line caps, giant-line truncation, binary/repeat folding; the vt100 parser is the only interpreter of escape sequences (no direct trust) | ✅ `memmux-pty` (SUM-10) |
| One task exhausts host memory | Global budget + admission reservations + pressure ladder react before swap thrashing | ✅ `memmux-sched` (SUM-9) |
| Adapter (third-party) compromises daemon | Least-privilege capability grants (worktree-scoped paths, no network/secrets by default); out-of-process execution flag | ⏳ grant modeled (`memmux-adapters::isolation`, SUM-75); enforced plugin runtime is Phase 5 |
| Worktree path escape | Worktrees live under a canonical managed root; capability grants gate filesystem access by prefix | ✅ `memmux-worktree::layout`, `CapabilityGrant::permits_path` |
| Daemon crash loses state | SQLite WAL durable store; crash recovery reconstructs all tasks on boot | ✅ `memmux-store`, `memmuxd::daemon::boot` (SUM-64/66) |

## Security checklist (per release)

- [x] Daemon socket is `0600` under a `0700` directory (test-asserted).
- [x] No secret material is persisted — references only (store + support bundle).
- [x] Every control decision is auditable with a reason + evidence.
- [x] Dirty worktrees are never removed without a preserved, hashed patch.
- [x] Owned process trees are recursively terminated; cleanup fraction is reported.
- [x] Unknown/escaped processes are surfaced, never hidden.
- [ ] Third-party adapters run out of process under a restricted runtime (Phase 5).
- [ ] Fuzzing of the vt100 parser and frame codec against hostile input (Phase 3 hardening).

## Non-goals (V2, §2.3)

- Kernel-level transparent checkpoint/restore of arbitrary binaries.
- Hard memory-safety guarantees for providers that bypass process boundaries or spawn
  privileged daemons.
