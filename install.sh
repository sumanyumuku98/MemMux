#!/bin/sh
# MemMux bootstrap installer.
#
#   curl -fsSL https://raw.githubusercontent.com/sumanyumuku98/MemMux/main/install.sh | sh
#
# Resolves the newest MemMux release (including pre-releases), downloads the archive for this
# OS/architecture, and installs `memmux` + `memmuxd`. Override behaviour with env vars:
#   MEMMUX_VERSION   pin a specific tag (e.g. v0.1.0) instead of the newest release
#   MEMMUX_BIN_DIR   install directory (default: $HOME/.local/bin)
set -eu

REPO="sumanyumuku98/MemMux"

# Resolve the target triple from uname.
os="$(uname -s)"
arch="$(uname -m)"
case "${os}-${arch}" in
  Linux-x86_64)   target="x86_64-unknown-linux-gnu" ;;
  Darwin-arm64)   target="aarch64-apple-darwin" ;;
  Darwin-x86_64)  target="x86_64-apple-darwin" ;;
  *) echo "memmux: unsupported platform ${os}-${arch}" >&2; exit 1 ;;
esac

# Resolve the release tag: an explicit MEMMUX_VERSION, else the newest release (the /releases
# list is newest-first and includes pre-releases, unlike /releases/latest).
tag="${MEMMUX_VERSION:-}"
if [ -z "${tag}" ]; then
  tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases?per_page=1" \
    | grep -m1 '"tag_name"' | cut -d'"' -f4)"
fi
if [ -z "${tag}" ]; then
  echo "memmux: could not determine the latest release tag" >&2
  exit 1
fi

bindir="${MEMMUX_BIN_DIR:-${HOME}/.local/bin}"
mkdir -p "${bindir}"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

url="https://github.com/${REPO}/releases/download/${tag}/memmux-${tag}-${target}.tar.gz"
echo "memmux: downloading ${tag} (${target})"
echo "        ${url}"
curl -fsSL "${url}" | tar xz -C "${tmp}"

install -m 0755 "${tmp}"/*/memmux "${bindir}/memmux"
install -m 0755 "${tmp}"/*/memmuxd "${bindir}/memmuxd"

echo "memmux: installed memmux and memmuxd to ${bindir}"
case ":${PATH}:" in
  *":${bindir}:"*) : ;;
  *) echo "memmux: add ${bindir} to your PATH:  export PATH=\"${bindir}:\$PATH\"" ;;
esac
