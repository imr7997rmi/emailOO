#!/usr/bin/env bash
# Fork helper: package a built binary as "emailOO.app", label it with
# the mod and its upstream base, and install it in /Applications and in the
# per-architecture build folder (~/Developer/emailOO-builds, outside iCloud). Usage:
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
staging="$target_dir/macos-$profile"
# EMAILOO_VARIANT=NYX installs a separate "emailOO NYX.app" (own bundle id
# and build folder), so an experimental branch never replaces the main app.
variant="${EMAILOO_VARIANT:-}"
build_dir="${EMAILOO_BUILD_DIR:-$HOME/Developer/emailOO-builds}/$arch_dir${variant:+-$variant}"
app_name="emailOO${variant:+ $variant}"
bundle_id="com.nivvoo.emailoo${variant:+.$(echo "$variant" | tr '[:upper:]' '[:lower:]')}"

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -1)"
base="$(git -C "$project_dir" rev-parse --short main 2>/dev/null || echo unknown)"
commit="$(git -C "$project_dir" rev-parse --short HEAD)"

bash "$project_dir/scripts/package-macos.sh" "$binary" "$staging" >/dev/null
app="$staging/Flectar Mail.app"
plist="$app/Contents/Info.plist"
# emailOO icon (SuitOO style) replaces the upstream artwork.
icon_png="$project_dir/resources/app-icon/emailoo-macos.png"
if [[ -f "$icon_png" ]]; then
  iconset="$staging/emailOO.iconset"
  rm -rf "$iconset" && mkdir -p "$iconset"
  for size in 16 32 128 256 512; do
    sips -z $size $size "$icon_png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$icon_png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  icon_file="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIconFile" "$plist")"
  iconutil -c icns "$iconset" -o "$app/Contents/Resources/${icon_file%.icns}.icns"
  rm -rf "$iconset"
fi
/usr/libexec/PlistBuddy \
  -c "Set :CFBundleIdentifier $bundle_id" \
  -c "Set :CFBundleName $app_name" \
  -c "Set :CFBundleDisplayName $app_name" \
  -c "Set :CFBundleShortVersionString $version mod" \
  -c "Set :CFBundleVersion $commit" \
  "$plist"
/usr/libexec/PlistBuddy -c "Delete :NSHumanReadableCopyright" "$plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :NSHumanReadableCopyright string imr7997rmi · base $base" "$plist"
# A stable local signing identity keeps keychain access across rebuilds;
# ad-hoc signatures change every build and macOS asks again per account.
# A self-signed certificate is enough (it does not need to be trusted).
sign_id=""
for candidate in ${FLECTAR_SIGN_ID:+"$FLECTAR_SIGN_ID"} "emailOO Local" "Nivvoo Dev"; do
  if security find-identity -p codesigning 2>/dev/null | grep -q "\"$candidate\""; then
    sign_id="$candidate"
    break
  fi
done
if [ -n "$sign_id" ]; then
  echo "signing with \"$sign_id\""
  codesign --force --deep --sign "$sign_id" "$app"
else
  echo "no local signing identity found; using ad-hoc signature" >&2
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

# Publish the build on GitHub Releases: one rolling pre-release per branch
# ("build-<branch>") with one zip per architecture, replaced on every build.
# EMAILOO_PUBLISH=0 skips it.
if [[ "${EMAILOO_PUBLISH:-1}" != 0 ]] && command -v gh >/dev/null 2>&1; then
  repo="$(git -C "$project_dir" remote get-url origin | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##')"
  branch="$(git -C "$project_dir" rev-parse --abbrev-ref HEAD)"
  tag="build-$(echo "$branch" | tr '/' '-')"
  asset="$staging/emailOO${variant:+-$variant}-$arch_dir-$profile.zip"
  rm -f "$asset"
  ditto -c -k --keepParent "$build_dir/$app_name.app" "$asset"
  line="- $arch_dir ($profile): $commit · $(date '+%Y-%m-%d %H:%M')"
  if git -C "$project_dir" push -q origin "HEAD:refs/heads/$branch" 2>/dev/null; then
    git -C "$project_dir" tag -f "$tag" HEAD >/dev/null
    git -C "$project_dir" push -q -f origin "refs/tags/$tag"
    if gh release view "$tag" -R "$repo" >/dev/null 2>&1; then
      notes="$(gh release view "$tag" -R "$repo" --json body -q .body | grep -v "^- $arch_dir " || true)"
      gh release edit "$tag" -R "$repo" --notes "$(printf '%s\n%s' "$notes" "$line")" >/dev/null
    else
      gh release create "$tag" -R "$repo" --prerelease --title "emailOO · $branch (última compilación)" \
        --notes "$(printf 'Compilación automática de la rama %s.\n\n%s' "$branch" "$line")" >/dev/null
    fi
    gh release upload "$tag" "$asset" -R "$repo" --clobber
    echo "published: https://github.com/$repo/releases/tag/$tag ($(basename "$asset"))"
  else
    echo "not published: could not push $branch to origin" >&2
  fi
fi
