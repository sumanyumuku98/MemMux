# memmux-bench

The MemMux competitive benchmark harness (Phase 0, §18). It drives a deterministic stub agent
through scenarios under a set of launchers, samples the live process tree with `memmux-metrics`,
and produces a Markdown report plus JSON Lines time series.

## CLI

```bash
memmux-bench scenarios          # list burst / soak / idle / leak
memmux-bench matrix             # §18.2 matrix size + runnable subset on this host
memmux-bench run --out bench-out
memmux-bench run --scenario leak --provider codex --intensity 2
memmux-bench run --include-competitors   # opt-in; installed competitors only
```

Internal subcommands `stub` / `stub-child` are used by launchers to execute a recording; you
don't invoke them directly.

## Modules

| Module | Responsibility | Story |
| --- | --- | --- |
| `stub` | Deterministic stub agent: `SessionRecording` (`simulate` + `execute`) | SUM-32 |
| `sampler` | Time-series sampling to JSONL + overhead accounting | SUM-33, SUM-31 |
| `launcher` | `raw-baseline` / `memmux` + opt-in competitor plugins | SUM-34, SUM-35 |
| `scenario` | burst / soak / idle / leak | SUM-36 |
| `report` | Markdown + Unicode-sparkline report | SUM-37 |
| `matrix` | §18.2 test-matrix enumeration | SUM-39 |
| `gates` | §18.5 launch-gate checks | SUM-40 |
| `run` | Live orchestration tying it together | — |

## Fairness

Competitor numbers are never fabricated. See
[`docs/benchmark-methodology.md`](../../docs/benchmark-methodology.md).
