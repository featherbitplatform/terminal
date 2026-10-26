#!/usr/bin/env bash
# Builds an ad-hoc-signed universal "featherbit terminal.app" and a .dmg.
# Usage: dev/package-macos.sh <version>     e.g. dev/package-macos.sh 0.1.0
# Output: target/dist/fbterm-<version>-macos-universal.dmg (+ .sha256)
set -euo pipefail

[[ "$(uname -s)" == "Darwin" ]] || { echo "package-macos.sh runs on macOS only" >&2; exit 1; }
version="${1:?usage: package-macos.sh <version>}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid version '$version' (expected X.Y.Z or X.Y.Z-pre)" >&2
  exit 1
fi
# Apple requires CFBundleVersion / ShortVersionString to be up to three integers.
build="${version%%[-+]*}"
[[ "$build" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "invalid base version '$build'" >&2; exit 1; }

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
dist="$root/target/dist"
app_name="featherbit terminal"
dmg="fbterm-${version}-macos-universal.dmg"
stage="$dist/stage"
app="$stage/$app_name.app"

for t in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --locked --release -p fbterm --target "$t"
done

rm -rf "$stage" "$dist/$dmg" "$dist/$dmg.sha256"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

lipo -create \
  "target/aarch64-apple-darwin/release/fbterm" \
  "target/x86_64-apple-darwin/release/fbterm" \
  -output "$app/Contents/MacOS/fbterm"
sed -e "s/__BUILD__/${build}/g" -e "s/__VERSION__/${version}/g" crates/fbterm/macos/Info.plist > "$app/Contents/Info.plist"
cp crates/fbterm/macos/fbterm.icns "$app/Contents/Resources/fbterm.icns"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Ad-hoc signature: required for Apple Silicon to launch. Not a Developer ID
# signature; the app is still unsigned for distribution (Gatekeeper warns).
codesign --force --deep -s - "$app"
codesign --verify --deep --strict "$app"

ln -s /Applications "$stage/Applications"
# hdiutil is the standard path. Some newer macOS releases fail it with
# "Resource busy"; locally (not in CI) fall back to diskutil. CI never falls back.
hdiutil_err="$dist/hdiutil.err"
if ! hdiutil create -volname "featherbit terminal" -srcfolder "$stage" -format UDZO "$dist/$dmg" >/dev/null 2>"$hdiutil_err"; then
  cat "$hdiutil_err" >&2
  if [[ -n "${CI:-}" ]]; then
    echo "hdiutil create failed" >&2
    exit 1
  fi
  echo "hdiutil create failed; falling back to diskutil image create (local only)" >&2
  rm -f "$dist/$dmg"
  diskutil image create from --format UDZO --volumeName "featherbit terminal" "$stage" "$dist/$dmg" >/dev/null
fi
rm -f "$hdiutil_err"

# Verify the image and its contents.
hdiutil verify "$dist/$dmg" >/dev/null
mnt="$(mktemp -d)"
cleanup() { hdiutil detach "$mnt" -quiet 2>/dev/null || true; rmdir "$mnt" 2>/dev/null || true; }
trap cleanup EXIT
hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dist/$dmg" >/dev/null
[[ -d "$mnt/$app_name.app" ]] || { echo "dmg is missing $app_name.app" >&2; exit 1; }
[[ -L "$mnt/Applications" ]] || { echo "dmg is missing the Applications symlink" >&2; exit 1; }
hdiutil detach "$mnt" -quiet
trap - EXIT
rmdir "$mnt" 2>/dev/null || true
(cd "$dist" && shasum -a 256 "$dmg" > "$dmg.sha256")
rm -rf "$stage"

echo "$dist/$dmg"
cat "$dist/$dmg.sha256"
