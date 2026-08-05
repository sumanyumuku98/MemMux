---
title: "Phase 1 — Memory-safe multiplexer"
description: "Daemon, scheduler, worktrees, provider adapters, and the TUI."
---


**Goal (§20.1):** TUI, tasks, Git worktrees, process groups, global budget, queue, cleanup.
**Exit criterion:** three active worktrees without uncontrolled daemon/log growth.

Phase 1 turns the Phase-0 instrumentation into a working local runtime: a daemon that owns
tasks and their durable state, a memory budget and admission scheduler, bounded terminal
capture, Git worktree orchestration, provider adapters, recursive process ownership, and a
dense terminal UI.

## What shipped

| Epic | Crate(s) | Highlights |
| --- | --- | --- |
| SUM-8 Task model & state machine | `memmux-core` | `Task` runtime record + audited transitions; legal-transition table (property-tested); idle/blocked/waiting sub-state classification |
| SUM-9 Memory budget & scheduler | `memmux-sched` | Resource envelope; reservation model; EWMA peak predictor; §7.4 scoring + admission queue; §7.5 pressure ladder |
| SUM-10 PTY & bounded capture | `memmux-pty` | Bounded ring buffer (10M-line test), repeated-line collapse, giant-line truncation, output governance, zstd history chunks, vt100 screen, portable-pty session |
| SUM-11 Git worktree orchestration | `memmux-worktree` | Collision-resistant slugs, repo mutation lock, dirty-state protection + verify-before-delete, conflict detection, merge/cherry-pick/retain/discard |
| SUM-12 Daemon core | `memmux-store`, `memmuxd` | SQLite WAL store; event log with cursors; explainable decisions; framed-JSON UDS API; crash recovery; support bundle |
| SUM-13 Provider adapters | `memmux-adapters` | `ProviderAdapter` + capability negotiation; generic/Claude/Codex adapters; least-privilege grants; PTY-backed `RuntimeInstance` |
| SUM-20 Process ownership & security | `memmux-metrics`, `memmuxd` | Recursive subtree termination (live-tested); reconciliation sweep; `0600`/`0700` socket; threat model |
| SUM-18 Terminal UI | `memmux` | Elm-architecture dashboard / tasks / queue / timeline / new-task form / help, headless-tested with ratatui `TestBackend` |

## Try it

```bash
cargo build --workspace

# Run the daemon (durable state under ~/.memmux, or set MEMMUX_ROOT)
memmuxd serve &

# Create/list tasks over the socket
memmuxd create --title "Refactor auth" --repo ~/src/product --provider claude-code
memmuxd list
memmuxd pressure

# Open the terminal UI
memmux
```

The daemon reconstructs all tasks after a restart (crash recovery), and `memmuxd support-bundle`
exports a redacted diagnostics archive.

## Architecture note — local API transport

The spec sketches tonic gRPC over the Unix socket. Because `protoc` is not available in the
build environment (and vendoring it would add a fragile C toolchain step to CI), the local API
is a **versioned, length-prefixed JSON protocol** over the UDS, defined in `memmux-proto`. It
serves the same §16.1 intent (typed local API, least-privilege socket, CLI round-trips) and is
swappable to gRPC later without changing call sites (aligns with the Phase-6 wire-protocol
story). See the [threat model](../../design/threat-model/) for the security posture.

## Deferred within Phase 1

- Gemini CLI / OpenCode adapters (SUM-73/74) land in Phase 2 alongside checkpoint/resume.
- Live bidirectional PTY *attach streaming* through the daemon is modeled in the TUI but its
  streaming endpoint is minimal in Phase 1.
