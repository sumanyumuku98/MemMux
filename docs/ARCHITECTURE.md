# MemMux architecture

This document tracks the implemented architecture as it grows phase by phase. It is derived
from the MemMux V2 technical specification; section numbers below reference that spec.

## Control-plane shape (§5)

MemMux is a **daemon-authoritative** system. The daemon (`memmuxd`) owns all durable state,
scheduling, and process lifecycle. Every UI (TUI, VS Code bridge, CLI, SDK) is a replaceable
client that talks to the daemon over a local gRPC-on-Unix-domain-socket transport. Provider
CLIs are *supervised workloads*, never owners of runtime state.

```
Clients (TUI / bridge / CLI / SDK)
        │  local gRPC / UDS  (memmux-proto)
   ┌────▼──────────────────────────────────────┐
   │ memmuxd: Scheduler · Lifecycle · Optimizer │
   │          Policy · Audit                    │
   └───┬──────────┬───────────────┬─────────────┘
  Provider    Worktree        Shared services
  adapters    manager      (repo index · MCP gateway)
        │          │               │
   Owned process groups / cgroups / PTYs
```

## Crate map

| Crate | Role | Phase introduced |
| --- | --- | --- |
| `memmux-core` | Domain types: identifiers, `TaskSpec`, `TaskState` machine, `Event` model | 0 |
| `memmux-metrics` | Process sampling (`/proc`, `libproc`) + task attribution | 0 |
| `memmux-bench` | Competitive benchmark harness: stub agent, scenarios, reports | 0 |
| `memmux-proto` | Versioned client/daemon protocol contract | 0 (skeleton) → 1 |
| `memmuxd` | Control-plane daemon | 0 (CLI skeleton) → 1 |
| `memmux` | Terminal UI client | 0 (skeleton) → 1 |

## Phase 0 — instrumentation

The Phase 0 goal (§20.1) is **accurate process attribution on macOS and Linux for two
providers**. The load-bearing crate is `memmux-metrics`:

- **Sampling** (`ProcessSampler`): a platform trait with a Linux `/proc` backend (VmRSS +
  `smaps_rollup` PSS) and a macOS `libproc` backend (`proc_pidinfo` resident size +
  `proc_pid_rusage` `phys_footprint`). The pure `/proc` parsers are compiled and unit-tested
  on every host so correctness is not gated on running Linux.
- **Tree** (`ProcessTree`): parent/child index with subtree, ancestry, and nearest-root
  queries, hardened against cycles.
- **Attribution** (`attribute`): reconciles the observed tree against declared task and
  shared-service roots plus wrapper-reported ownership, classifying every process as
  **owned**, **shared**, **escaped**, or **unknown** (§14.2). `AttributionReport` exposes the
  `attributed_fraction` that backs the ≥95% launch gate (§18.5).

The `memmux-bench` harness wraps this to answer the §18 benchmark questions (memory
attribution, sampling overhead, bounded growth) across scenarios and competing launchers.

## Design invariants carried forward

- **Bounded by default** — the `Event` model stores payloads out of line behind a
  `bounded_payload_ref`; nothing in the hot path may re-read an unbounded transcript (§8.3).
- **Own what you launch** — attribution must map ≥95% of sampled private RSS to a task or a
  declared shared service; the remainder is surfaced, never hidden (§18.5).
- **Logical task over physical process** — `TaskState` distinguishes residency from existence
  so a task survives queueing, recycling, and hibernation (§6).
