#!/bin/sh
# Stores the signing files `eas credentials` downloaded (credentials.json and credentials/ios/)
# as the EAS production secrets .ci/sign-ios.sh reads: CI_IOS_P12 and EXACT_PROFILE (files) and
# CI_IOS_P12_PASSWORD. Run from remote/: sh tools/upload-signing.sh
# The password never reaches the terminal. Delete credentials/ and credentials.json afterwards.
set -eu
test -f credentials.json || { echo "No credentials.json here: download it with bunx eas credentials -p ios first." >&2; exit 1; }
read_json() { bun -e "const c = await Bun.file('credentials.json').json(); console.log($1)"; }
p12=$(read_json 'c.ios.distributionCertificate.path')
password=$(read_json 'c.ios.distributionCertificate.password')
profile=$(read_json 'c.ios.provisioningProfilePath')
test -f "$p12" && test -f "$profile"
bunx eas env:set production --name CI_IOS_P12 --type file --value "$p12" --visibility secret --non-interactive
bunx eas env:set production --name EXACT_PROFILE --type file --value "$profile" --visibility secret --non-interactive
bunx eas env:set production --name CI_IOS_P12_PASSWORD --type string --value "$password" --visibility secret --non-interactive >/dev/null
echo "Stored CI_IOS_P12, EXACT_PROFILE and CI_IOS_P12_PASSWORD in EAS's production environment."
