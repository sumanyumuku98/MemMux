# Benchmark methodology & fairness

This document governs how the `memmux-bench` harness measures MemMux and its alternatives, and
the claims discipline it follows (§18, §19.5 of the MemMux V2 specification). It exists so that
every number the harness emits is reproducible and defensible.

## What we measure

The harness answers the §18.1 benchmark questions relevant to Phase 0:

- **Attribution** — of the process tree MemMux launches, how much can it map back to the owning
  task? Reported as *tree-attributed fraction* (target ≥ 95%, §18.5).
- **Bounded memory** — does resident memory stay flat under output volume and session age?
  Reported as soak growth vs a 100 MiB limit.
- **Sampling overhead** — what does one process-tree sample cost, relative to a realistic
  steady-state daemon cadence? Reported as a fraction of a 1 s cadence (target ≤ 2%).

## The stub agent

Real coding agents are non-deterministic, so the harness drives a **deterministic stub**
([`SessionRecording`]). A recording is an ordered script of allocate / free / emit / leak /
sleep / spawn-child steps. Because it is deterministic:

- the same recording produces the same modeled trajectory every time (`simulate`), giving exact
  expected values for tests; and
- executed for real (`execute`), it allocates touched pages, emits output, and spawns child
  worker processes, so `memmux-metrics` samples a genuine multi-process tree.

Provider profiles (`claude-code`, `codex`, …) only set a plausible baseline footprint; they do
**not** claim to reproduce a specific provider's real behaviour.

## Launchers and claims discipline

- **`raw-baseline`** and **`memmux`** run the *identical* stub. In Phase 0, before the daemon
  exists, the MemMux launcher is an honest apples-to-apples spawn — it does not yet apply
  admission or optimization, and the report does not pretend otherwise.
- **Competitor launchers** (`dmux`, `cmux`, `herdr`, `agentmux`) are **opt-in**
  (`--include-competitors`) and only run when their binary is actually installed. MemMux cannot
  invoke each competitor's bespoke CLI correctly without a validated command template; the
  built-in templates are best-effort placeholders. Running a competitor with a guessed CLI
  would produce misleading numbers, so:
  - the default benchmark omits competitors entirely;
  - when included, the harness prints a warning that their numbers are indicative only; and
  - absent tools are **omitted, never estimated** — echoing the spec's "no public control
    found" stance (§19.5). We never fabricate a competitor data point.

## Sampling

- Memory is accounted using the platform's best proportional metric — PSS on Linux
  (`smaps_rollup`), `phys_footprint` on macOS (`proc_pid_rusage`) — falling back to RSS. This
  avoids counting shared pages multiple times.
- Each sample records its own collection cost. The overhead gate divides mean sample cost by a
  1 s reference cadence (a realistic daemon sampling rate), not the much tighter benchmark
  interval, so the number reflects steady-state cost rather than a stress interval.

## Reproducibility

- All recordings are written to the output directory as JSON; all samples are written as JSON
  Lines. A report can be regenerated from the committed JSONL without re-running.
- The test matrix (§18.2) is enumerated in `matrix.rs`; the harness reports the full matrix
  size and the subset runnable on the current host (matching OS, sufficient memory, installed
  products), so partial local results are never mistaken for full-matrix coverage.

## Gate honesty

Gates that cannot be measured until a later phase (crash-safety, cleanup, resume, pressure
avoidance) are reported as **skipped** with the phase that will supply the evidence — never as
passed. `all_measured_gates_pass` ignores skipped gates; it only fails on a measured failure.

[`SessionRecording`]: ../crates/memmux-bench/src/stub.rs
