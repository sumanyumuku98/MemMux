# MemMux paper — two builds

*"MemMux: Runtime Verification and Honest Resource Attribution for Fleets of Parallel Coding Agents"*
(Sumanyu Muku, New York University).

Both builds share one preamble (`preamble.tex`), body (`body.tex`), and bibliography (`refs.bib`);
only the thin wrapper differs:

| Build | Wrapper | Mode | Author |
|---|---|---|---|
| **arXiv preprint** | `arxiv.tex` | `neurips_2026` `[preprint]` | **named** (Sumanyu Muku, NYU) |
| **NeurIPS workshop** | `workshop.tex` | `neurips_2026` `[dblblindworkshop]` | **anonymized** (double-blind) |

Target: *"Who Verifies the Agents?"* NeurIPS 2026 workshop — regular paper (4–9 pp excl. refs/appendix),
double-blind, non-archival, deadline **2026-08-29 AoE**.

## Build

`tectonic` (recommended — fetches packages, runs all passes, uses the local `neurips_2026.sty`):

```sh
tectonic arxiv.tex      # -> arxiv.pdf     (named preprint)
tectonic workshop.tex   # -> workshop.pdf  (anonymized, double-blind)
```

or classic TeX Live: `pdflatex <build> && bibtex <build> && pdflatex <build> && pdflatex <build>`
(the `neurips_2026.sty` is included, so no download needed), or upload to Overleaf.

## Files
- `body.tex` — shared paper body (abstract → conclusion + bibliography). The architecture diagram is
  drawn in TikZ and every chart is drawn inline with `pgfplots` from the measured data (no image
  files).
- `preamble.tex` — shared preamble for both builds: packages, the colour palette, the reusable
  `pgfplots` chart style, the code-listing style, and the TikZ diagram styles + `\title`.
- `arxiv.tex`, `workshop.tex` — the two thin wrappers (documentclass + `neurips_2026` option,
  `\input{preamble}`, author/`\repourl`).
- `refs.bib` — 13 references, all real and verified.
- `neurips_2026.sty` — the official NeurIPS 2026 style (bundled so the builds are self-contained).
- `linux-run/` — provenance for the numbers: `host.json`, the generated `report.md`, and the SVG
  figures from the reference run.

## Numbers

All tables and Figure 1 come from the `memmux-bench` harness (`memmux-bench paper`) on the reference
host recorded in `linux-run/host.json`: an AWS `m7i.2xlarge` — Intel Xeon Platinum 8488C, ≈31 GiB RAM,
Ubuntu 24.04 (Linux 6.17), `tmux` 3.4, MemMux 0.8.0. Regenerate on any host with the same command
(see the paper's Reproducibility section / Appendix A).

## Notes
- The workshop build withholds the repository URL to preserve double-blind review; the arXiv build
  links it. Nothing else in `body.tex` is de-anonymizing.
- Non-archival + dual-submission is explicitly allowed, so the named arXiv preprint and the anonymized
  workshop submission can coexist.
