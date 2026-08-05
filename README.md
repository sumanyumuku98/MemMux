<div align="center">
  <img src="assets/icon.png" alt="MemMux" width="128" height="128" />

  # MemMux

  **The memory-aware local runtime for parallel AI coding agents.**

  [![CI](https://github.com/sumanyumuku98/MemMux/actions/workflows/ci.yml/badge.svg)](https://github.com/sumanyumuku98/MemMux/actions/workflows/ci.yml)
  [![Docs](https://github.com/sumanyumuku98/MemMux/actions/workflows/docs.yml/badge.svg)](https://sumanyumuku98.github.io/MemMux/)

  **📖 Full documentation: [sumanyumuku98.github.io/MemMux](https://sumanyumuku98.github.io/MemMux/)**
  [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
</div>

---

## What is MemMux?

Existing tools multiplex terminals, panes, and Git worktrees. **MemMux multiplexes
constrained execution resources**: memory, processes, repository services, tool servers,
workspaces, and agent execution slots. It keeps many *logical* tasks available while
controlling how many *physical* workloads remain resident.

Run several coding agents (Claude Code, Codex, Gemini CLI, OpenCode, …) each in its own Git
worktree, without letting session age or parallelism drive your workstation into swap.

The category MemMux defines is **enforcement, not just observability**: showing memory usage
is useful, but controlling admission, resident state, process lifecycles, and active-session
footprint is the product.

## Architecture

```
 Clients: TUI | Desktop shell | VS Code bridge | CLI | SDK
                      │ local gRPC / Unix domain socket
 ┌────────────────────▼─────────────────────────────────┐
 │                  MemMux Daemon (memmuxd)              │
 │  Scheduler │ Lifecycle │ Memory Optimizer │ Audit     │
 └──────┬───────────┬──────────────────┬────────────────┘
   Provider     Worktree           Shared Services
   Adapters     Manager            (repo index, MCP GW)
        │           │                    │
 ┌──────▼───────────▼────────────────────▼──────────────┐
 │      Owned process groups / cgroups / PTYs           │
 └───────────────────────────────────────────────────────┘
```

- **`memmuxd`** — authoritative Rust + Tokio daemon (state, scheduling, lifecycle, audit).
- **`memmux`** — dense Ratatui/Crossterm TUI client.
- **`memmux-metrics`** — cross-platform process accounting & attribution (Phase 0 core).
- **`memmux-bench`** — competitive benchmark harness (stub agent, scenarios, reports).
- **`memmux-core`** — shared domain types (tasks, state machine, events).
- **`memmux-lifecycle`** — pure lifecycle logic (checkpoints, safe-points, recycling, resume).
- **`memmux-proto`** — versioned client/daemon protocol types.

## Non-functional launch gates

| Gate | Threshold |
| --- | --- |
| Idle footprint | daemon + TUI < **180 MB** idle RSS |
| Sampling overhead | < **2% CPU** at 20 managed tasks |
| UI latency | < **250 ms** p95 update latency |
| No lost work | **zero** Git-state loss across crashes / kills / reboot |
| Cleanup | **≥ 99.5%** of owned descendants gone within 10 s of termination |
| Resume | **≥ 99%** native-resume success for supported providers |
| Attribution | **≥ 95%** of sampled private RSS mapped to a task or shared service |

## Roadmap

MemMux is built in seven phases (tracked in Linear, mirrored here):

| Phase | Scope | Exit criterion |
| --- | --- | --- |
| **0 — Instrumentation prototype** | Process accounting, attribution, bounded terminal capture, benchmark harness | Accurate attribution on macOS/Linux for two providers |
| **1 — Memory-safe multiplexer** | TUI, tasks, worktrees, process groups, global budget, queue, cleanup | Three active worktrees without uncontrolled growth |
| **2 — Lifecycle runtime** | Checkpoint, hibernate, native resume, recycling, more adapters | Reliable resume, no lost dirty state under faults |
| 3 — Active optimization | Incremental transcripts, MCP leases, lazy services, recycling | Measured active-session memory reduction vs Phase 1 |
| 4 — Shared repository services | Base index, per-worktree overlays, search/symbol APIs | Lower baseline across three worktrees |
| 5 — Ecosystem | VS Code bridge, SDK, policy hierarchy, desktop shell | Third party adds a provider without daemon changes |
| 6 — Distributed extension | Optional remote workers, resource placement | Local product proven, protocol stable |

See the [documentation site](https://sumanyumuku98.github.io/MemMux/) for per-phase design notes
and the [architecture overview](https://sumanyumuku98.github.io/MemMux/design/architecture/).

## Building

```bash
# Requires a stable Rust toolchain (1.82+)
cargo build --workspace
cargo test  --workspace
cargo fmt   --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Try it

```bash
cargo build --workspace

# Run the daemon (durable state under ~/.memmux; override with MEMMUX_ROOT)
cargo run -p memmuxd -- serve &

# Create and list tasks over the Unix socket
cargo run -p memmuxd -- create --title "Refactor auth" --repo ~/src/product --provider claude-code
cargo run -p memmuxd -- list
cargo run -p memmuxd -- pressure

# Open the terminal UI
cargo run -p memmux

# Phase 0 tooling is still here too:
cargo run -p memmuxd -- snapshot --root 1
cargo run -p memmux-bench -- run --scenario all --provider claude-code --out bench-out
```

See the docs site: [Phase 0](https://sumanyumuku98.github.io/MemMux/phases/phase-0/),
[Phase 1](https://sumanyumuku98.github.io/MemMux/phases/phase-1/),
[Architecture](https://sumanyumuku98.github.io/MemMux/design/architecture/), and
[Threat model](https://sumanyumuku98.github.io/MemMux/design/threat-model/).

## Status

🚧 Early development; APIs are unstable. **Phases 0–2 are complete**: process accounting and
attribution (Phase 0); the memory-safe multiplexer — daemon, scheduler, bounded capture,
worktrees, provider adapters, process ownership, and the TUI (Phase 1); and the lifecycle
runtime — checkpoint/hibernate, native + reconstructed resume, RSS-threshold recycling with a
reclaimed-bytes ledger, and Gemini CLI + OpenCode adapters (Phase 2, see
[the Phase 2 notes](https://sumanyumuku98.github.io/MemMux/phases/phase-2/)). Phase 3 (active
optimization) is next.

## License

MIT © 2026 Sumanyu Muku. See [LICENSE](./LICENSE).
