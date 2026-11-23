#!/bin/sh
# Prepares a clean Ubuntu runner to build Magnitor's Linux host: clones Exact at $EXACT_REF next
# to the app (where exact.mjs and package.json's file: dependencies expect it), applies the Linux
# patches, installs the pinned Rust toolchain and the host Hermes bundle, and fetches the crates
# Exact's lock names, since Exact's tools build offline.
# The Linux twin of prepare-macos.sh (Exact's docs/reference.md, "Native CI preparation").
set -eu

: "${EXACT_REF:?set EXACT_REF to the Exact commit this app builds against}"

export EXACT="$(cd "$PWD/.." && pwd)/exact"
if [ ! -d "$EXACT/.git" ]; then
  git clone --filter=blob:none https://github.com/expo/exact.git "$EXACT"
fi
git -C "$EXACT" fetch --quiet origin "$EXACT_REF" || true
git -C "$EXACT" checkout --quiet --detach "$EXACT_REF"

# Fixes to Exact that Magnitor's Linux host needs and $EXACT_REF lacks, as .patch files in
# .ci/exact-patches-linux.
# linux-tray-shortcuts.patch: the status item as a StatusNotifierItem in the desktop's tray (KDE;
# GNOME with the AppIndicator extension) whose popover is a window of its own, the global keyboard
# shortcut through the GlobalShortcuts portal, and the desktop's interface font from the Settings
# portal. Made with `git diff` of a working Exact checkout. Drop it once Exact includes it; a
# patch that no longer applies (after bumping EXACT_REF) stops the build here.
for patch in "$PWD"/.ci/exact-patches-linux/*.patch; do
  [ -e "$patch" ] || continue
  echo "Applying $(basename "$patch") to Exact"
  git -C "$EXACT" apply --whitespace=nowarn "$patch"
done

bun install --frozen-lockfile --cwd "$EXACT"

if ! command -v rustup >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "${RUNNER_TEMP:-/tmp}/rustup.sh"
  sh "${RUNNER_TEMP:-/tmp}/rustup.sh" -y --profile minimal --default-toolchain none --no-modify-path
fi
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"

pin=$(bun -e 'console.log(Bun.TOML.parse(await Bun.file(process.env.EXACT + "/rust-toolchain.toml").text()).toolchain.channel)')
triple=x86_64-unknown-linux-gnu
rustup toolchain install "$pin" --profile minimal --target "$triple"
rustup default "$pin"
cargo +"$pin" run --locked --manifest-path "$EXACT/vendor/ibex/crates/hermes-lean-sys-installer/Cargo.toml" -- --target "$triple"
cargo +"$pin" fetch --locked --manifest-path "$EXACT/Cargo.toml"

# Hand EXACT and PATH to the later steps.
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "EXACT=$EXACT" >> "$GITHUB_ENV"
  echo "${CARGO_HOME:-$HOME/.cargo}/bin" >> "$GITHUB_PATH"
fi
