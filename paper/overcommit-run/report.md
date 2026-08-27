# Binding-budget overcommit run (H4) — provenance

- Measured (UTC): 2026-08-27
- Host: AWS m7i.xlarge, Intel Xeon Platinum 8488C, 4 vCPU, 15.3 GiB RAM + 12 GiB swap, Ubuntu 24.04 (Linux 6.x)
- memmux/bench 0.9.0; tmux 3.4; herdr/dmux/cmux/agentmux skipped (not installed)
- Scenario: overcommit, N=12 agents, per-agent footprint MEMMUX_BENCH_HOLD_MIB=1400 (~1.5 GiB tree, ~= Standard-class reservation prior)
- MemMux agent budget: 7500 MiB (binding: below the ~15 GiB the ungoverned fleet uses)
- Trials: 5; mean +/- 95% CI half-width (1.96 sigma / sqrt(n))

## Bounded footprint (H4a)

| Launcher | Peak total (MiB) | Steady total (MiB) | Peak swap (MiB) | Governance actions |
| --- | ---: | ---: | ---: | --- |
| raw-baseline | 15074.5 +/- 13.3 | 15068.7 +/- 12.7 | 1882 | n/a (ungoverned) |
| tmux 3.4 | 15347.4 +/- 535.2 | 15069.1 +/- 24.2 | 2165 | n/a (ungoverned) |
| memmux 0.9.0 | 7479.0 +/- 0.1 | 4512.3 +/- 43.9 | 0 | 19 (7 deferrals + 12 reclamations) |

Attribution 100% for every launcher (external oracle). Manager CPU under active governance ~14.8% (pressure-ladder work; distinct from the H5 monitoring cost measured on the hold sweep). Swap = peak sum of per-process swap_bytes over the sampling window.

## No-lost-work (H4b)
memmux: 0 emergency terminations this run (admission + hibernate/recycle kept footprint under budget without an emergency terminate), so checkpoint-before-terminate remained unit-tested, not exercised e2e. Baselines: unsupported (no checkpoint mechanism).
