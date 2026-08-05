# Phase 0 — Instrumentation prototype

**Goal (§20.1):** wrap agents, collect descendant-process and memory data, bounded terminal
capture. **Exit criterion:** accurate process attribution on macOS and Linux for two providers.

Phase 0 is complete when MemMux can launch agent workloads, sample their full process trees,
and correctly attribute every process back to the owning task — with a benchmark harness that
proves it and reports honestly.

## What shipped

| Epic | Deliverable | Where |
| --- | --- | --- |
| Scaffold / CI (SUM-5) | Cargo workspace, six crates, GitHub Actions CI (fmt+clippy+test on macOS+Linux) | `Cargo.toml`, `.github/workflows/ci.yml` |
| Process accounting (SUM-6) | `ProcessSampler` (Linux `/proc`+PSS, macOS `libproc`+`phys_footprint`), `ProcessTree`, owned/shared/escaped/unknown attribution | `crates/memmux-metrics` |
| Benchmark harness (SUM-7) | Deterministic stub agent, burst/soak/idle/leak scenarios, JSONL sampler, launchers, §18.2 matrix, §18.5 gates, Markdown+sparkline reports | `crates/memmux-bench` |

## Attribution: the exit criterion

`memmux-metrics` reconstructs the OS process tree and reconciles it against the roots MemMux
launched plus the descendants its wrappers reported (§14.2):

- **owned** — inside a task's subtree;
- **shared** — inside a declared shared-service subtree;
- **escaped** — reported for a task but found outside its subtree (surfaced, not hidden);
- **unknown** — neither.

`AttributionReport::attributed_fraction` backs the ≥ 95% launch gate (§18.5). The harness
measures *launched-tree attribution* — of the processes MemMux started (a stub plus its spawned
child worker), what fraction the engine maps back to the task — which runs at 100% in the smoke
benchmark.

## Running it

```bash
# Build everything
cargo build --workspace

# Take a one-shot attribution snapshot against a pid (defaults to self)
cargo run -p memmuxd -- snapshot --root 1

# List scenarios; show the §18.2 matrix and what is runnable here
cargo run -p memmux-bench -- scenarios
cargo run -p memmux-bench -- matrix

# Run the benchmark (baseline vs MemMux) and write a report + JSONL
cargo run -p memmux-bench -- run --scenario all --provider claude-code --out bench-out
cat bench-out/report.md
```

Competitor launchers are opt-in and only run if installed:

```bash
cargo run -p memmux-bench -- run --include-competitors --scenario soak
```

See [benchmark-methodology.md](./benchmark-methodology.md) for the fairness rules and
[ARCHITECTURE.md](./ARCHITECTURE.md) for the crate map.

## Gate status (Phase 0 smoke run)

| Gate | Status |
| --- | --- |
| Attribution ≥ 95% | ✅ measured (100% of launched tree) |
| Sampling overhead ≤ 2% | ✅ measured (< 0.2% at 1 s cadence) |
| Bounded memory (< 100 MiB soak growth) | ✅ measured |
| No lost work / Cleanup / Pressure / Resume | ⚪ skipped — evidence arrives in Phases 1–2 |
