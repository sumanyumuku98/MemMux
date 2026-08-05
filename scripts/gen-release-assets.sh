#!/usr/bin/env bash
# Generate the curl installer (install.sh) and the Homebrew formula (memmux.rb) from the built
# archives' checksums (SUM-25). Usage: gen-release-assets.sh <tag> <dist-dir>
set -euo pipefail

tag="$1"
dist="$2"
repo="sumanyumuku98/MemMux"
version="${tag#v}"
base="https://github.com/${repo}/releases/download/${tag}"

sha_for() {
  awk '{print $1}' "${dist}/memmux-${tag}-$1.tar.gz.sha256"
}
linux_x64="$(sha_for x86_64-unknown-linux-gnu)"
mac_arm="$(sha_for aarch64-apple-darwin)"
mac_x64="$(sha_for x86_64-apple-darwin)"

# --- curl installer ------------------------------------------------------------------------
# Outer ${...} expand now (repo/tag); runtime shell vars are escaped as \$ to stay literal.
cat >"${dist}/install.sh" <<EOS
#!/usr/bin/env sh
# MemMux installer: curl -fsSL ${base}/install.sh | sh
set -e
REPO="${repo}"
TAG="${tag}"

os="\$(uname -s)"
arch="\$(uname -m)"
case "\$os-\$arch" in
  Linux-x86_64)   target="x86_64-unknown-linux-gnu" ;;
  Darwin-arm64)   target="aarch64-apple-darwin" ;;
  Darwin-x86_64)  target="x86_64-apple-darwin" ;;
  *) echo "unsupported platform: \$os-\$arch" >&2; exit 1 ;;
esac

bindir="\${MEMMUX_BIN_DIR:-\$HOME/.local/bin}"
mkdir -p "\$bindir"
tmp="\$(mktemp -d)"
url="https://github.com/\$REPO/releases/download/\$TAG/memmux-\$TAG-\$target.tar.gz"
echo "downloading \$url"
curl -fsSL "\$url" | tar xz -C "\$tmp"
install -m 0755 "\$tmp"/*/memmux "\$bindir/memmux"
install -m 0755 "\$tmp"/*/memmuxd "\$bindir/memmuxd"
rm -rf "\$tmp"
echo "installed memmux and memmuxd to \$bindir"
echo "ensure \$bindir is on your PATH"
EOS
chmod +x "${dist}/install.sh"

# --- Homebrew formula ----------------------------------------------------------------------
cat >"${dist}/memmux.rb" <<EOR
class Memmux < Formula
  desc "Memory-aware local runtime for parallel AI coding agents"
  homepage "https://github.com/${repo}"
  version "${version}"
  license "MIT"

  on_macos do
    on_arm do
      url "${base}/memmux-${tag}-aarch64-apple-darwin.tar.gz"
      sha256 "${mac_arm}"
    end
    on_intel do
      url "${base}/memmux-${tag}-x86_64-apple-darwin.tar.gz"
      sha256 "${mac_x64}"
    end
  end

  on_linux do
    url "${base}/memmux-${tag}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "${linux_x64}"
  end

  def install
    bin.install "memmux", "memmuxd"
  end

  test do
    system "#{bin}/memmuxd", "version"
  end
end
EOR

echo "generated ${dist}/install.sh and ${dist}/memmux.rb"
