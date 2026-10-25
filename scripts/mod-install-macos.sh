#!/usr/bin/env bash
# Fork helper: package a built binary as "emailOO.app", label it with
# the mod and its upstream base, and install it in /Applications and in the
# per-architecture build folder. Usage:
#   scripts/mod-install-macos.sh [debug|release]
set -euo pipefail

profile="${1:-debug}"
project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$project_dir/target}"
binary="$target_dir/$profile/flectar-mail"
case "$(uname -m)" in
  arm64) arch_dir="apple-silicon" ;;
  *) arch_dir="intel" ;;
esac
build_dir="$project_dir/../build/$arch_dir"
staging="$target_dir/macos-$profile"
app_name="emailOO"

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -1)"
base="$(git -C "$project_dir" rev-parse --short main 2>/dev/null || echo unknown)"
commit="$(git -C "$project_dir" rev-parse --short HEAD)"

bash "$project_dir/scripts/package-macos.sh" "$binary" "$staging" >/dev/null
app="$staging/Flectar Mail.app"
plist="$app/Contents/Info.plist"
/usr/libexec/PlistBuddy \
  -c "Set :CFBundleIdentifier com.nivvoo.emailoo" \
  -c "Set :CFBundleName $app_name" \
  -c "Set :CFBundleDisplayName $app_name" \
  -c "Set :CFBundleShortVersionString $version mod" \
  -c "Set :CFBundleVersion $commit" \
  "$plist"
/usr/libexec/PlistBuddy -c "Delete :NSHumanReadableCopyright" "$plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :NSHumanReadableCopyright string imr7997rmi · base $base" "$plist"
# A stable local signing identity keeps keychain access across rebuilds;
# ad-hoc signatures change every build and macOS asks again per account.
sign_id="${FLECTAR_SIGN_ID:-emailOO Local}"
if security find-identity -p codesigning 2>/dev/null | grep -q "\"$sign_id\""; then
  codesign --force --deep --sign "$sign_id" "$app"
else
  echo "signing identity \"$sign_id\" not found; using ad-hoc signature" >&2
  codesign --force --deep --sign - "$app" >/dev/null 2>&1
fi

osascript -e "quit app \"$app_name\"" >/dev/null 2>&1 || true
for _ in $(seq 1 20); do
  pgrep -f "$app_name.app/Contents/MacOS" >/dev/null || break
  sleep 1
done

mkdir -p "$build_dir"
for destination in "$build_dir/$app_name.app" "/Applications/$app_name.app"; do
  rm -rf "$destination"
  ditto "$app" "$destination"
  codesign --verify --deep "$destination"
  echo "installed: $destination"
done
# Keep Launch Services on the /Applications copy only.
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
"$lsregister" -u "$build_dir/$app_name.app" >/dev/null 2>&1 || true
open "/Applications/$app_name.app"
