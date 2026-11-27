# Sourced by the bundle step (Exact's docs/reference.md, "Build Android artifacts without a
# device"): the upload keystore as EXACT_KEYSTORE (a file path), with EXACT_KEYSTORE_PASSWORD,
# EXACT_KEY_ALIAS and EXACT_KEY_PASSWORD.
#
# The keystore is EAS's own for this app (`eas credentials -p android`), which the step writes to
# EAS_BUILD_CREDENTIALS_FILE from `eas.job.secrets.buildCredentials`: `keystore.dataBase64`,
# `keystorePassword`, `keyAlias` and `keyPassword`. A secret file EXACT_KEYSTORE and secret strings
# EXACT_KEYSTORE_PASSWORD, EXACT_KEY_ALIAS and EXACT_KEY_PASSWORD in EAS's production environment
# are used instead when set.
# Ephemeral workers only; no shell tracing here.
set -eu
if [ -z "${EXACT_KEYSTORE:-}" ]; then
  # Beside the credentials file, which the step removes once the bundle is built.
  CI_SIGN_DIR=$(dirname "${EAS_BUILD_CREDENTIALS_FILE:?EAS sent no credentials file}")
  CI_SIGN_DIR="$CI_SIGN_DIR" bun -e '
    const fs = require("fs");
    const file = process.env.EAS_BUILD_CREDENTIALS_FILE;
    const text = file && fs.existsSync(file) ? fs.readFileSync(file, "utf8").trim() : "";
    let all = {};
    try { all = text ? JSON.parse(text) : {}; } catch { console.error("error: EAS’s build credentials are not JSON."); process.exit(1); }
    const k = all.keystore;
    if (!k || !k.dataBase64) {
      console.error("error: EAS sent no upload keystore. Set one up with bunx eas credentials -p android, and keep withoutCredentials out of eas.json.");
      process.exit(1);
    }
    const dir = process.env.CI_SIGN_DIR;
    const field = (name) => k[name] ?? all[name] ?? "";
    fs.writeFileSync(`${dir}/upload.jks`, Buffer.from(k.dataBase64, "base64"), { mode: 0o600 });
    fs.writeFileSync(`${dir}/keystore-password`, field("keystorePassword"), { mode: 0o600 });
    fs.writeFileSync(`${dir}/key-alias`, field("keyAlias"), { mode: 0o600 });
    fs.writeFileSync(`${dir}/key-password`, field("keyPassword"), { mode: 0o600 });
    console.log("Signing with EAS’s upload keystore.");
  '
  EXACT_KEYSTORE="$CI_SIGN_DIR/upload.jks"
  EXACT_KEYSTORE_PASSWORD=$(cat "$CI_SIGN_DIR/keystore-password")
  EXACT_KEY_ALIAS=$(cat "$CI_SIGN_DIR/key-alias")
  EXACT_KEY_PASSWORD=$(cat "$CI_SIGN_DIR/key-password")
fi
test -f "$EXACT_KEYSTORE"
test -n "$EXACT_KEYSTORE_PASSWORD"
test -n "$EXACT_KEY_ALIAS"
test -n "$EXACT_KEY_PASSWORD"
export EXACT_KEYSTORE EXACT_KEYSTORE_PASSWORD EXACT_KEY_ALIAS EXACT_KEY_PASSWORD
