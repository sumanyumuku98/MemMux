#!/usr/bin/env bash
# macOS code-signing + notarization for the release binaries (SUM-26).
#
# Invoked by the release workflow ONLY when the Apple secrets are configured; without them the
# step is skipped and unsigned archives are shipped. Required environment (from repo secrets):
#   APPLE_CERTIFICATE            base64-encoded Developer ID Application .p12
#   APPLE_CERTIFICATE_PASSWORD   password for the .p12
#   APPLE_SIGNING_IDENTITY       e.g. "Developer ID Application: Name (TEAMID)"
#   APPLE_NOTARY_KEY             base64-encoded App Store Connect API .p8 key
#   APPLE_NOTARY_KEY_ID          the key id
#   APPLE_NOTARY_ISSUER          the issuer id
#
# Usage: macos-notarize.sh <binary> [<binary> ...]
set -euo pipefail

keychain="memmux-build.keychain"
keychain_pw="$(openssl rand -hex 16)"
security create-keychain -p "$keychain_pw" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$keychain_pw" "$keychain"
security default-keychain -s "$keychain"

echo "$APPLE_CERTIFICATE" | base64 --decode >cert.p12
security import cert.p12 -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_pw" "$keychain" >/dev/null

for bin in "$@"; do
  echo "codesigning $bin"
  codesign --force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$bin"
done

# Notarize the signed binaries as a zip and wait for Apple's verdict.
echo "$APPLE_NOTARY_KEY" | base64 --decode >notary_key.p8
zip -j memmux-notarize.zip "$@"
xcrun notarytool submit memmux-notarize.zip \
  --key notary_key.p8 \
  --key-id "$APPLE_NOTARY_KEY_ID" \
  --issuer "$APPLE_NOTARY_ISSUER" \
  --wait

# Standalone CLI binaries can't be stapled (no bundle); the notarization ticket is published
# to Apple and validated by Gatekeeper on first run.
rm -f cert.p12 notary_key.p8 memmux-notarize.zip
security delete-keychain "$keychain" || true
echo "signed + notarized: $*"
