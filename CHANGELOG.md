# Changelog

All notable changes to MemMux are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 releases (`v0.*`) are published
as GitHub **pre-releases**; APIs are still unstable.

## [0.9.0] — 2026-08-25

The verification slice: the memory contract is now proven with numbers, and written up.

### Added
- **Verification benchmark suite (H1–H5).** `memmux-bench` measures each guarantee against
  `raw`/`tmux`/`herdr` on identical workloads: attribution (H1), complete cleanup on teardown
  (H2), escaped-process visibility (H3), bounded footprint + no-lost-work under overcommit (H4),
  and monitoring overhead (H5) — with trials + 95% CIs, real CPU%, and embedded SVG plots
  (#55–#59).
- **One-command reproducer** `memmux-bench paper` + N-sweep and fairness-at-scale: runs the full
  paper matrix into a self-contained artifact dir (JSONL + host-stamped `report.md` + `host.json`
  + figures) (#60).
- **Real per-process accounting**: PSS/USS/swap/major-faults with provider-vs-manager tagged JSONL
  (#55).
- **Real-agent mode** `memmux-bench run --agent-cmd` / `--agent-cwd`: drive a real coding agent
  (e.g. `claude -p ...`) through the identical harness instead of the stub (#63).
- **Paper** (`paper/`): NeurIPS-template dual build — a named arXiv preprint and a double-blind
  workshop submission from one shared body — with a 32 GB Linux reference run and a real-agent
  validation of live Claude Code sessions (#61, #62, #65).

### Changed
- README refreshed for the enforcement + verification story, the H1–H5 signals, the reproducer,
  and the real-agent example (#64).

## [0.8.0] — 2026-08-09

### Added
- Honest **MemMux-vs-`tmux`-vs-`herdr`** benchmark harness with manager-overhead broken out from
  provider footprint, and skip-with-reason fairness discipline (#53).

## [0.7.0] — 2026-08-09

The scheduler-enforcement slice: measure → admit → reclaim, end to end.

### Added
- **Budget-gated admission**: a reservations ledger + scoring queue that admits the highest-scoring
  tasks that fit the footprint budget and defers the rest with a reason (#51).
- **Pressure ladder**: graduated reclamation under memory pressure — reclaim idle children,
  hibernate/recycle by policy, and checkpoint-before-terminate to preserve dirty Git state (#50).

## [0.6.0] — 2026-08-07

### Added
- Real per-task memory **sampling + attribution** wired into the daemon's ~1 Hz tick (#47).

### Fixed
- **Recursive termination + escaped-process reconciliation**: a terminated task's entire process
  subtree is reaped (SIGTERM → grace → SIGKILL) before runtime stop, so descendants can't reparent
  to init and leak; escaped children are surfaced as events (#48).

## [0.5.0] – [0.5.4] — 2026-08-06/07

### Added
- **Herdr-style pane multiplexer**: agents open as live, colored TUI panes; one-key launch/close,
  reliable restart, folder browser (#33, #35).
- Resizable pane splits, a two-section sidebar with per-workspace pane groups, a configurable pane
  leader (default `Ctrl-b`), and mouse click-to-focus (#34, #37, #40, #41).

### Fixed
- Pane splits now launch into and group with the focused pane's workspace (#43, #45); rustdoc
  intra-doc links resolved and `cargo doc` enforced in CI (#39).

## [0.4.0] — 2026-08-05

### Added
- `memmux update` self-update with an update-available hint (#31).

## [0.3.0] — 2026-08-05

### Added
- Modern TUI theme + Herdr-style workspace sidebar (#29); true interactive attach via raw PTY
  passthrough (#28).

### Fixed
- Release pipeline guards against a tag/version mismatch (#27).

## [0.2.0] — 2026-08-05

### Added
- **Per-task Git worktrees** in the task lifecycle (#25); frictionless agent launch (default repo
  to cwd, optional title) (#24); a daemon-managed workspaces view (#22, #23); daemon auto-start for
  single-command startup (#21).

### Fixed
- Installer bootstrap that works for pre-releases (#20).

## [0.1.0] — 2026-08-05

Initial pre-release. Phase 0 (process accounting, attribution, benchmark harness) and Phase 1
(daemon core over a Unix socket, durable SQLite-WAL store, Git-worktree orchestration, PTY +
bounded terminal capture, task model & scheduler, provider adapters, the Ratatui TUI) plus the
Phase 2 lifecycle runtime (checkpoint/hibernate, native + reconstructed resume, recycling) and the
release pipeline + docs site.

[0.9.0]: https://github.com/sumanyumuku98/MemMux/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/sumanyumuku98/MemMux/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/sumanyumuku98/MemMux/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/sumanyumuku98/MemMux/compare/v0.5.4...v0.6.0
[0.5.4]: https://github.com/sumanyumuku98/MemMux/compare/v0.4.0...v0.5.4
[0.4.0]: https://github.com/sumanyumuku98/MemMux/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/sumanyumuku98/MemMux/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/sumanyumuku98/MemMux/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sumanyumuku98/MemMux/releases/tag/v0.1.0
