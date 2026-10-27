#!/usr/bin/env bash
# Builds fbterm for Linux and packages it as .deb, .rpm and .tar.gz.
# Runs on Linux, natively for the given architecture (no cross-compiling). Release builds run
# in an almalinux:9 container: its glibc (2.34, RHEL 9's) is the oldest of the supported
# systems, so the binary runs on all of them. The script refuses a binary that asks for more.
# Needs cargo-deb and cargo-generate-rpm (the versions pinned in .github/workflows/release.yml).
# Usage: dev/package-linux.sh <version> <arch>     e.g. dev/package-linux.sh 0.1.0 x86_64
# Output: target/dist/fbterm-<version>-linux-<arch>.{deb,rpm,tar.gz} (+ .sha256 each)
set -euo pipefail

[[ "$(uname -s)" == "Linux" ]] || { echo "package-linux.sh runs on Linux only" >&2; exit 1; }
version="${1:?usage: package-linux.sh <version> <x86_64|aarch64>}"
arch="${2:?usage: package-linux.sh <version> <x86_64|aarch64>}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid version '$version' (expected X.Y.Z or X.Y.Z-pre)" >&2
  exit 1
fi
case "$arch" in
  x86_64|aarch64) ;;
  *) echo "invalid arch '$arch' (expected x86_64 or aarch64)" >&2; exit 1 ;;
esac
[[ "$(uname -m)" == "$arch" ]] || { echo "this machine is $(uname -m), not $arch (no cross-compiling)" >&2; exit 1; }
for tool in cargo-deb cargo-generate-rpm objdump ar rpm desktop-file-validate; do
  command -v "$tool" >/dev/null || { echo "package-linux.sh needs $tool" >&2; exit 1; }
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
target="${CARGO_TARGET_DIR:-$root/target}"
dist="$root/target/dist"
name="fbterm-${version}-linux-${arch}"
stage="$dist/stage-linux-$arch"
linux="crates/fbterm/linux"

# Debian and RPM sort "~" before anything, so a pre-release precedes its release; RPM versions
# may not contain "-".
deb_version="${version//-/\~}"
rpm_version="${version//-/\~}"

cargo build --locked --release -p fbterm
bin="$target/release/fbterm"
"$bin" --version | grep -qx "fbterm [0-9].*" || { echo "fbterm --version printed nothing usable" >&2; exit 1; }
# The .deb declares libc6 (>= 2.34) and the .rpm needs RHEL 9's glibc 2.34.
newest_glibc="$(objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
if [[ "$(printf '%s\n' "$newest_glibc" GLIBC_2.34 | sort -V | tail -1)" != GLIBC_2.34 ]]; then
  echo "the binary needs $newest_glibc; build it on glibc 2.34 (almalinux:9)" >&2
  exit 1
fi

mkdir -p "$dist"
rm -rf "$stage" "$dist/$name".{deb,rpm,tar.gz}{,.sha256}

# The packages ship the binary exactly as cargo built it (the release profile), the same
# file in all three.
cargo deb -p fbterm --no-build --no-strip --deb-version "$deb_version" --output "$dist/$name.deb" >/dev/null
cargo generate-rpm -p crates/fbterm --target-dir "$target" \
  --set-metadata "version = \"$rpm_version\"" --output "$dist/$name.rpm"

# Tarball: the same files under one top folder, plus the install note and the license.
top="$stage/$name"
install -Dm755 "$bin" "$top/bin/fbterm"
install -Dm644 "$linux/fbterm.desktop" "$top/share/applications/fbterm.desktop"
for s in 32 64 128 256 512; do
  install -Dm644 "$linux/icons/hicolor/${s}x${s}/apps/fbterm.png" \
    "$top/share/icons/hicolor/${s}x${s}/apps/fbterm.png"
done
install -Dm644 "$linux/INSTALL" "$top/INSTALL"
install -Dm644 LICENSE "$top/LICENSE"
tar -C "$stage" --owner=0 --group=0 --numeric-owner --sort=name -czf "$dist/$name.tar.gz" "$name"

# Verify each package holds the binary, the desktop entry and every icon.
want=(usr/bin/fbterm usr/share/applications/fbterm.desktop)
for s in 32 64 128 256 512; do want+=("usr/share/icons/hicolor/${s}x${s}/apps/fbterm.png"); done
# No dpkg on the build image: read the .deb's data member with ar and tar.
data="$(ar t "$dist/$name.deb" | grep '^data\.tar')"
mkdir -p "$stage"
ar p "$dist/$name.deb" "$data" > "$stage/$data"
deb_files="$(tar -tf "$stage/$data")"
rm "$stage/$data"
tar_files="$(tar -tzf "$dist/$name.tar.gz")"
for f in "${want[@]}"; do
  grep -qx "\./$f" <<<"$deb_files" || { echo "deb is missing /$f" >&2; exit 1; }
  grep -qx "$name/${f#usr/}" <<<"$tar_files" || { echo "tarball is missing ${f#usr/}" >&2; exit 1; }
done
rpm_files="$(rpm -qlp "$dist/$name.rpm")"
for f in "${want[@]}"; do
  grep -qx "/$f" <<<"$rpm_files" || { echo "rpm is missing /$f" >&2; exit 1; }
done
desktop-file-validate "$linux/fbterm.desktop"
rm -rf "$stage"

(cd "$dist" && for ext in deb rpm tar.gz; do sha256sum "$name.$ext" > "$name.$ext.sha256"; done)
for ext in deb rpm tar.gz; do
  echo "$dist/$name.$ext"
  cat "$dist/$name.$ext.sha256"
done
