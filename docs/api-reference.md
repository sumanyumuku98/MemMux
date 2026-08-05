# API reference (rustdoc)

The full generated API documentation for every MemMux crate is published alongside this guide:

**→ [Open the rustdoc API reference](api/memmuxd/index.html)**

It is built with `cargo doc --workspace --no-deps` on every push to `main`, so it always matches
the current code. Notable crates:

- [`memmuxd`](api/memmuxd/index.html) — the control-plane daemon (state, scheduling, lifecycle).
- [`memmux_core`](api/memmux_core/index.html) — shared domain types (tasks, state machine, events).
- [`memmux_lifecycle`](api/memmux_lifecycle/index.html) — checkpoints, safe-points, recycling, resume.
- [`memmux_sched`](api/memmux_sched/index.html) — memory budget and admission scheduler.
- [`memmux_adapters`](api/memmux_adapters/index.html) — provider adapters and capability negotiation.
- [`memmux_pty`](api/memmux_pty/index.html) — bounded PTY capture and history.
- [`memmux_metrics`](api/memmux_metrics/index.html) — process accounting and attribution.
- [`memmux_worktree`](api/memmux_worktree/index.html) — Git worktree orchestration.
- [`memmux_store`](api/memmux_store/index.html) — the durable SQLite store.
- [`memmux_proto`](api/memmux_proto/index.html) — the client/daemon wire protocol.
