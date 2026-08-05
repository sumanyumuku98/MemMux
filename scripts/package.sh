#!/usr/bin/env bash
# Package the release binaries for one target into a tar.gz + sha256 (SUM-25).
# Usage: package.sh <tag> <target>
set -euo pipefail

tag="$1"
target="$2"
name="memmux-${tag}-${target}"
bindir="target/${target}/release"

mkdir -p "$name"
cp "$bindir/memmux" "$bindir/memmuxd" "$name/"
cp README.md LICENSE "$name/"
tar czf "${name}.tar.gz" "$name"

if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "${name}.tar.gz" >"${name}.tar.gz.sha256"
else
  sha256sum "${name}.tar.gz" >"${name}.tar.gz.sha256"
fi

echo "packaged ${name}.tar.gz"
