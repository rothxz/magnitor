#!/bin/sh
# Prepares a clean macOS runner to build Magnitor: clones Exact at $EXACT_REF next to the app
# (where exact.mjs and package.json's file: dependencies expect it), installs the pinned Rust
# toolchain and the host Hermes bundle, and fetches the crates Exact's lock names, since
# Exact's tools build offline.
# Adapted from Exact's docs/reference.md, "Native CI preparation".
set -eu

: "${EXACT_REF:?set EXACT_REF to the Exact commit this app builds against}"

export EXACT="$(cd "$PWD/.." && pwd)/exact"
if [ ! -d "$EXACT/.git" ]; then
  git clone --filter=blob:none https://github.com/expo/exact.git "$EXACT"
fi
git -C "$EXACT" fetch --quiet origin "$EXACT_REF" || true
git -C "$EXACT" checkout --quiet --detach "$EXACT_REF"

# Fixes to Exact that Magnitor needs and $EXACT_REF lacks, as .patch files in .ci/exact-patches.
# status-item-popover.patch: a status item's popover is AppKit's NSPopover at the menu bar
# button. Without it the popover is painted in the app's window, which an accessory app never
# shows, so clicking the menu bar icon opens nothing. Drop a patch once Exact includes it; a
# patch that no longer applies (after bumping EXACT_REF) stops the build here.
for patch in "$PWD"/.ci/exact-patches/*.patch; do
  [ -e "$patch" ] || continue
  echo "Applying $(basename "$patch") to Exact"
  git -C "$EXACT" apply --whitespace=nowarn "$patch"
done

bun install --frozen-lockfile --cwd "$EXACT"

if ! command -v rustup >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$RUNNER_TEMP/rustup.sh"
  sh "$RUNNER_TEMP/rustup.sh" -y --profile minimal --default-toolchain none --no-modify-path
fi
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"

pin=$(bun -e 'console.log(Bun.TOML.parse(await Bun.file(process.env.EXACT + "/rust-toolchain.toml").text()).toolchain.channel)')
triple=aarch64-apple-darwin
rustup toolchain install "$pin" --profile minimal --target "$triple"
rustup default "$pin"
cargo +"$pin" run --locked --manifest-path "$EXACT/vendor/ibex/crates/hermes-lean-sys-installer/Cargo.toml" -- --target "$triple"
cargo +"$pin" fetch --locked --manifest-path "$EXACT/Cargo.toml"

# Hand EXACT and PATH to the later steps.
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "EXACT=$EXACT" >> "$GITHUB_ENV"
  echo "${CARGO_HOME:-$HOME/.cargo}/bin" >> "$GITHUB_PATH"
fi
