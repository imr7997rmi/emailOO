#!/usr/bin/env bash
# Fork helper: build the app for Apple silicon and Intel, join both binaries
# into one universal binary and install/package it with mod-install-macos.sh.
# Usage:
#   scripts/mod-build-universal-macos.sh [debug|release]
# The usual EMAILOO_VARIANT / EMAILOO_PUBLISH / EMAILOO_BUILD_DIR variables
# are passed through to mod-install-macos.sh.
set -euo pipefail
trap 'status=$?; printf "universal macOS build failed at line %s: %s\n" "$LINENO" "$BASH_COMMAND" >&2; exit "$status"' ERR

profile="${1:-release}"
case "$profile" in
  release) profile_args=(--release) ;;
  debug) profile_args=() ;;
  *)
    printf 'Unsupported profile: %s (use debug or release)\n' "$profile" >&2
    exit 1
    ;;
esac

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$project_dir/target}"
triples=(aarch64-apple-darwin x86_64-apple-darwin)

cd "$project_dir"
for triple in "${triples[@]}"; do
  if ! rustup target list --installed | grep -qx "$triple"; then
    printf 'Missing Rust target %s; run: rustup target add %s\n' "$triple" "$triple" >&2
    exit 1
  fi
  cargo build --locked "${profile_args[@]}" --target "$triple" --bin flectar-mail
done

mkdir -p "$target_dir/$profile"
universal_binary="$target_dir/$profile/flectar-mail"
lipo -create \
  "$target_dir/${triples[0]}/$profile/flectar-mail" \
  "$target_dir/${triples[1]}/$profile/flectar-mail" \
  -output "$universal_binary"
printf 'Universal binary: %s\n' "$(lipo -archs "$universal_binary")"

EMAILOO_ARCH=universal bash "$project_dir/scripts/mod-install-macos.sh" "$profile"
