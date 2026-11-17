#!/bin/sh
# Imports the Developer ID Application certificate and notarization credentials into a
# temporary keychain, for `exact release` (sign, notarize, staple, package).
# Use only on an ephemeral runner. Never turn on shell tracing (set -x) around this.
#
# Needs: MACOS_DEVELOPER_ID_P12 (base64 .p12), MACOS_DEVELOPER_ID_P12_PASSWORD,
#        APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD (app-specific password).
# Optional: MACOS_PROVISION_PROFILE (base64 Developer ID provisioning profile, needed to
#           sign the desktop widget's App Group).
set -eu

for name in MACOS_DEVELOPER_ID_P12 MACOS_DEVELOPER_ID_P12_PASSWORD APPLE_ID APPLE_TEAM_ID APPLE_APP_PASSWORD; do
  eval "value=\${$name:-}"
  if [ -z "$value" ]; then
    echo "::error::Missing secret $name. See the 'Release signing' section of .github/workflows/macos.yml."
    exit 1
  fi
done

dir="$RUNNER_TEMP/signing"
mkdir -p "$dir"
keychain="$dir/build.keychain-db"
keychain_password=$(openssl rand -hex 24)

printf '%s' "$MACOS_DEVELOPER_ID_P12" | base64 --decode > "$dir/developer-id.p12"
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$dir/developer-id.p12" -k "$keychain" -P "$MACOS_DEVELOPER_ID_P12_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_password" "$keychain" >/dev/null
security list-keychains -d user -s "$keychain"
security default-keychain -d user -s "$keychain"
rm -f "$dir/developer-id.p12"

identity=$(security find-identity -v -p codesigning "$keychain" | awk '/"Developer ID Application: / {print $2}')
if [ "$(printf '%s\n' "$identity" | grep -c .)" != 1 ]; then
  echo "::error::Expected exactly one Developer ID Application identity in the .p12."
  exit 1
fi

# `exact release` reads notarytool credentials from this keychain profile.
xcrun notarytool store-credentials exact-notary \
  --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" \
  --keychain "$keychain" >/dev/null

{
  echo "EXACT_DEVELOPER_ID=$identity"
  echo "EXACT_NOTARY_PROFILE=exact-notary"
  echo "CI_KEYCHAIN=$keychain"
} >> "$GITHUB_ENV"

if [ -n "${MACOS_PROVISION_PROFILE:-}" ]; then
  printf '%s' "$MACOS_PROVISION_PROFILE" | base64 --decode > "$dir/developer-id.provisionprofile"
  echo "EXACT_MAC_PROFILE=$dir/developer-id.provisionprofile" >> "$GITHUB_ENV"
fi
