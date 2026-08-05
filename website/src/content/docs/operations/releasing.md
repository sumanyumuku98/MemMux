---
title: "Releasing MemMux"
description: "Packaging, signing, distribution, and hosting the docs."
---


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

## Running the daemon as a service

Each archive includes optional service units under `packaging/` so `memmuxd` can run in the
background and restart on failure:

```bash
# Linux (systemd --user)
mkdir -p ~/.config/systemd/user
cp packaging/systemd/memmuxd.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now memmuxd

# macOS (launchd user agent)
cp packaging/launchd/com.memmux.memmuxd.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.memmux.memmuxd.plist
```

### Homebrew tap (auto-published)

The generated `memmux.rb` is attached to each release **and** auto-committed to the
`sumanyumuku98/homebrew-tap` repo's `Formula/memmux.rb` by the release workflow — but only when a
push token is configured. To enable it:

1. Create the tap repo `sumanyumuku98/homebrew-tap` (public, empty is fine).
2. Create a fine-grained PAT with **Contents: read/write** on that repo.
3. Add it to the MemMux repo as the `HOMEBREW_TAP_TOKEN` secret
   (**Settings → Secrets and variables → Actions**).

With the secret present, `brew install sumanyumuku98/tap/memmux` works after each release. Until
the secret exists, the publish step is skipped (the formula still ships as a release asset, so
`brew install ./memmux.rb` from a downloaded formula continues to work).

## Hosted docs (GitHub Pages)

`.github/workflows/docs.yml` builds the complete documentation site on every push to `main` and
deploys it to GitHub Pages. It is live today at
**https://sumanyumuku98.github.io/MemMux/** and comprises:

- an **Astro Starlight** site built from the Markdown in `website/src/content/docs/` (this guide);
- the generated **rustdoc API reference** nested under **`/api/`** (`cargo doc --workspace
  --no-deps`, built with `-D warnings`).

Pages is already enabled (Source: "GitHub Actions"). The Starlight toolchain (Astro + Starlight)
is pinned to mature versions in `website/package.json` with a committed `package-lock.json`, and
CI installs it with `npm ci` (org dependency policy).

### Custom domain (optional, later)

To serve the docs from a custom domain instead of the `/MemMux` project path: point the domain's
DNS at GitHub Pages, set it under **Settings → Pages → Custom domain**, and in
`website/astro.config.mjs` set `site` to the domain and `base: '/'` (so the site serves at the
domain root). Then repoint the README badge/link and the repo description/website accordingly.

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
