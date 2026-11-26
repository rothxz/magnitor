# Sourced by the archive step (Exact's docs/reference.md, "Import an iOS signing identity on a CI
# worker"): the distribution certificate in a temporary keychain, exported as EXACT_IDENTITY, and
# the App Store profile's file as EXACT_PROFILE.
#
# The credentials are EAS's own for this app (`eas credentials`), which the step writes to
# EAS_BUILD_CREDENTIALS_FILE from `eas.job.secrets.buildCredentials`: one target, its certificate
# and profile in base64. Secret files CI_IOS_P12, CI_IOS_P12_PASSWORD and EXACT_PROFILE in EAS's
# production environment (tools/upload-signing.sh) are used instead when set.
# Ephemeral workers only; no shell tracing here.
set -eu
CI_SIGN_DIR=$(mktemp -d)
CI_KEYCHAIN="$CI_SIGN_DIR/build.keychain-db"
CI_KEYCHAIN_PASSWORD=$(openssl rand -hex 24)
# The step's own exit status survives the clean-up.
trap 'status=$?; security delete-keychain "$CI_KEYCHAIN" 2>/dev/null || true; rm -rf "$CI_SIGN_DIR"; exit $status' EXIT
if [ -z "${CI_IOS_P12:-}" ]; then
  CI_SIGN_DIR="$CI_SIGN_DIR" bun -e '
    const fs = require("fs");
    const file = process.env.EAS_BUILD_CREDENTIALS_FILE;
    const text = file && fs.existsSync(file) ? fs.readFileSync(file, "utf8").trim() : "";
    let all = {};
    try { all = text ? JSON.parse(text) : {}; } catch { console.error("error: EAS’s build credentials are not JSON."); process.exit(1); }
    const targets = Object.keys(all);
    if (targets.length === 0) {
      console.error("error: EAS sent no signing credentials. Set them up with bunx eas credentials -p ios (an App Store distribution certificate and profile), and keep withoutCredentials out of eas.json.");
      process.exit(1);
    }
    if (targets.length > 1) { console.error(`error: EAS sent credentials for several targets (${targets.join(", ")}); Magnitor Remote has one.`); process.exit(1); }
    const t = all[targets[0]], dir = process.env.CI_SIGN_DIR;
    fs.writeFileSync(`${dir}/dist.p12`, Buffer.from(t.distributionCertificate.dataBase64, "base64"));
    fs.writeFileSync(`${dir}/app.mobileprovision`, Buffer.from(t.provisioningProfileBase64, "base64"));
    fs.writeFileSync(`${dir}/p12-password`, t.distributionCertificate.password ?? "", { mode: 0o600 });
    console.log(`Signing with EAS’s credentials for the target ${targets[0]}.`);
  '
  CI_IOS_P12="$CI_SIGN_DIR/dist.p12"
  CI_IOS_P12_PASSWORD=$(cat "$CI_SIGN_DIR/p12-password")
  # CI_SIGN_DIR lasts until the step ends (the trap runs at its exit), so the build reads it.
  EXACT_PROFILE="$CI_SIGN_DIR/app.mobileprovision"
  export EXACT_PROFILE
fi
security create-keychain -p "$CI_KEYCHAIN_PASSWORD" "$CI_KEYCHAIN"
security set-keychain-settings -lut 21600 "$CI_KEYCHAIN"
security unlock-keychain -p "$CI_KEYCHAIN_PASSWORD" "$CI_KEYCHAIN"
security import "$CI_IOS_P12" -k "$CI_KEYCHAIN" -P "$CI_IOS_P12_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$CI_KEYCHAIN_PASSWORD" "$CI_KEYCHAIN" >/dev/null
security list-keychains -d user -s "$CI_KEYCHAIN"
EXACT_IDENTITY=$(security find-identity -v -p codesigning "$CI_KEYCHAIN" | awk '/[0-9A-F]{40}/ {print $2}')
test "$(printf '%s\n' "$EXACT_IDENTITY" | wc -l | tr -d ' ')" = 1
test -n "$EXACT_IDENTITY"
export EXACT_IDENTITY
