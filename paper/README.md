# MemMux paper (arXiv-style preprint)

Source for *"MemMux: Runtime Verification and Honest Resource Attribution for Fleets of Parallel
Coding Agents"* (Sumanyu Muku, New York University).

## Files
- `main.tex` — the paper. Self-contained: standard `article` class + widely available packages
  (`pgfplots`, `natbib`, `booktabs`, `hyperref`); all figures are drawn inline from the measured
  data, so there are no external image files to manage.
- `refs.bib` — bibliography (all references are real and verifiable).

## Build

With **tectonic** (recommended — fetches packages and runs all passes automatically):

```sh
tectonic main.tex        # -> main.pdf
```

With a classic **TeX Live / MacTeX** toolchain:

```sh
pdflatex main
bibtex   main
pdflatex main
pdflatex main
```

Or upload `main.tex` + `refs.bib` to **Overleaf** and compile.

## Numbers

Every table and figure comes from the `memmux-bench` harness (`memmux-bench paper`), which also
writes `host.json` and the raw JSONL time series. The values in this preprint are a **preliminary,
single-host** evaluation (Apple M4 Max, 36 GiB, macOS); regenerate on a Linux reference host with the
same command to extend the swap/USS and larger-fleet results (see the paper's Reproducibility
section).

## Status

Named preprint. The double-blind workshop submission (e.g. NeurIPS "Who Verifies the Agents?") should
be re-typeset in the venue template and anonymized.
