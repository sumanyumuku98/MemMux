---
title: Installation
description: Install the MemMux binaries (memmux and memmuxd) on Linux or macOS.
---

MemMux ships prebuilt binaries for **Linux (x86_64)** and **macOS (Apple Silicon + Intel)** with
every [release](https://github.com/sumanyumuku98/MemMux/releases/latest). Two binaries are
installed: `memmuxd` (the control-plane daemon) and `memmux` (the terminal UI).

## One-line installer (recommended)

Installs `memmux` and `memmuxd` to `~/.local/bin`:

```bash
curl -fsSL https://github.com/sumanyumuku98/MemMux/releases/latest/download/install.sh | sh
```

Then make sure that directory is on your `PATH` (add the line to `~/.zshrc` or `~/.bashrc` to
persist it):

```bash
export PATH="$HOME/.local/bin:$PATH"
memmuxd version   # -> memmuxd 0.1.0 (protocol 0.1.0)
```

The installer detects your OS/architecture, downloads the matching archive, and installs both
binaries. Override the destination with `MEMMUX_BIN_DIR`, e.g.:

```bash
curl -fsSL https://github.com/sumanyumuku98/MemMux/releases/latest/download/install.sh \
  | MEMMUX_BIN_DIR=/usr/local/bin sh
```

Because the installer fetches over `curl` (not a browser), macOS Gatekeeper does not quarantine
the binaries — there is no signing prompt to clear.

## Manual download

Grab the archive for your platform from the
[releases page](https://github.com/sumanyumuku98/MemMux/releases/latest), verify its checksum,
and extract:

```bash
# pick your target: aarch64-apple-darwin | x86_64-apple-darwin | x86_64-unknown-linux-gnu
TARGET=aarch64-apple-darwin
VERSION=v0.1.0
base="https://github.com/sumanyumuku98/MemMux/releases/download/$VERSION"

curl -fsSLO "$base/memmux-$VERSION-$TARGET.tar.gz"
curl -fsSLO "$base/memmux-$VERSION-$TARGET.tar.gz.sha256"
shasum -a 256 -c "memmux-$VERSION-$TARGET.tar.gz.sha256"   # sha256sum -c on Linux

tar xzf "memmux-$VERSION-$TARGET.tar.gz"
mv memmux memmuxd ~/.local/bin/
```

On macOS, if you downloaded the archive through a **browser** (which sets the quarantine flag),
clear it before first run:

```bash
xattr -dr com.apple.quarantine ~/.local/bin/memmux ~/.local/bin/memmuxd
```

## Build from source

Always works, and the binaries are built by your own toolchain (no signing concerns):

```bash
git clone https://github.com/sumanyumuku98/MemMux && cd MemMux
cargo build --release          # requires a stable Rust toolchain (1.82+)
# binaries: target/release/memmux and target/release/memmuxd
```

## First run

```bash
memmuxd serve &                # daemon; durable state under ~/.memmux (or set MEMMUX_ROOT)

# A task you can try with no agent CLI installed — the generic provider runs any command:
memmuxd create --title "try it" --repo ~/some/git/repo \
  --provider generic -- sh -c 'for i in $(seq 1 50); do echo tick $i; sleep 1; done'
memmuxd list                   # copy the task id
memmuxd start <task-id>

memmux                         # open the terminal UI (press ? for keybindings)
```

If you have a provider CLI on your `PATH`, swap `--provider generic -- …` for
`--provider claude-code` (or `codex`, `gemini-cli`, `opencode`).

:::note[Pre-release notes]
- macOS binaries are **unsigned** (notarization is pending). The `curl` installer avoids
  Gatekeeper; a browser download needs the `xattr` step above.
- `brew install` is **not available yet** — the Homebrew tap is not wired up. Use the installer,
  a manual download, or build from source.
:::
