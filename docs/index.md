# MemMux

**The memory-aware local runtime for parallel AI coding agents.**

Existing tools multiplex terminals, panes, and Git worktrees. **MemMux multiplexes constrained
execution resources**: memory, processes, repository services, tool servers, workspaces, and
agent execution slots. It keeps many *logical* tasks available while controlling how many
*physical* workloads remain resident — so you can run several coding agents (Claude Code, Codex,
Gemini CLI, OpenCode, …) each in its own Git worktree without your machine falling over.

This site is the complete project documentation. Source and issues live on
[GitHub](https://github.com/sumanyumuku98/MemMux).

## Where to start

- **[Architecture](ARCHITECTURE.md)** — the crate map and how the daemon, TUI, scheduler, PTY
  capture, worktrees, adapters, and lifecycle fit together.
- **[Threat model](threat-model.md)** — the security boundaries MemMux enforces.
- **[Phases](phase-0.md)** — the build is staged; each phase page is a design + delivery record:
  - [Phase 0 — Instrumentation prototype](phase-0.md): process accounting, attribution, bounded
    capture, benchmark harness.
  - [Phase 1 — Memory-safe multiplexer](phase-1.md): daemon, scheduler, worktrees, adapters, TUI.
  - [Phase 2 — Lifecycle runtime](phase-2.md): checkpoint/hibernate, native + reconstructed
    resume, RSS-threshold recycling with a reclaimed-bytes ledger.
- **[API reference](api-reference.md)** — the generated rustdoc for every crate.

## Non-functional launch gates

| Gate | Threshold |
| --- | --- |
| Idle RSS | < 180 MB |
| CPU @ 20 tasks | < 2% |
| UI latency | < 250 ms p95 |
| Git-state loss | zero |
| Descendant cleanup | ≥ 99.5% within 10 s |
| Resume | ≥ 99% native-resume success for supported providers |

## Quick start

```bash
cargo build --workspace
memmuxd serve &          # durable state under ~/.memmux (or set MEMMUX_ROOT)
memmuxd create --title "Refactor auth" --repo ~/src/product --provider claude-code
memmux                   # open the terminal UI
```

See [Releasing](RELEASE.md) for packaging, signing, and distribution.
