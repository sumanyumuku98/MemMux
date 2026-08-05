# Releasing MemMux

MemMux ships as a single set of static-ish binaries (`memmux`, `memmuxd`) via a tag-triggered
GitHub Actions pipeline (`.github/workflows/release.yml`, SUM-25).

## Cutting a release

```bash
# From an up-to-date main:
git tag v0.1.0
git push origin v0.1.0
```

The tag push runs the **Release** workflow, which:

1. builds `--release` binaries for three targets — `x86_64-unknown-linux-gnu`,
   `aarch64-apple-darwin`, `x86_64-apple-darwin`;
2. packages each as `memmux-<tag>-<target>.tar.gz` with a `.sha256` (`scripts/package.sh`);
3. generates a curl installer (`install.sh`) and a Homebrew formula (`memmux.rb`) from the
   checksums (`scripts/gen-release-assets.sh`);
4. publishes a GitHub Release with all of the above and auto-generated notes.

> This is a hand-authored, dependency-policy-compliant pipeline that produces the same artifacts
> as `cargo-dist` (archives + Homebrew + curl installer). It can be swapped for `cargo-dist`
> later without changing the artifact contract.

## Install methods (after a release)

```bash
# curl installer
curl -fsSL https://github.com/sumanyumuku98/MemMux/releases/download/v0.1.0/install.sh | sh

# Homebrew (via a tap — see below)
brew install sumanyumuku98/tap/memmux
```

### Homebrew tap

The generated `memmux.rb` is attached to each release. To serve it via Homebrew, create a
`sumanyumuku98/homebrew-tap` repository and commit `memmux.rb` to it (or automate that copy in a
follow-up job using a PAT). Until the tap exists, users can `brew install ./memmux.rb` from a
downloaded formula.

## macOS code-signing & notarization (SUM-26 — pending your Apple credentials)

The release workflow signs and notarizes the macOS binaries **only when** the following repo
secrets are set (otherwise the step is skipped and unsigned archives ship):

| Secret | Meaning |
| --- | --- |
| `APPLE_CERTIFICATE` | base64-encoded Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | password for that `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_NOTARY_KEY` | base64-encoded App Store Connect API `.p8` key |
| `APPLE_NOTARY_KEY_ID` | the API key id |
| `APPLE_NOTARY_ISSUER` | the App Store Connect issuer id |

Add these under **Settings → Secrets and variables → Actions**. The signing/notarization flow is
implemented in `scripts/macos-notarize.sh` (create keychain → import cert → `codesign --options
runtime` → `notarytool submit --wait`). SUM-26 stays open until these secrets are provided and a
signed release is verified end to end, since it requires an Apple Developer account that this
build environment does not have.
