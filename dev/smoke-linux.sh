#!/usr/bin/env bash
# Installs one Linux package in a clean container and checks it, as the release workflow's
# smoke jobs do: a minimal install (no recommended or weak dependencies), `fbterm --version`,
# the desktop entry and icons, then removal.
# Usage: dev/smoke-linux.sh <deb|rpm|tar.gz> <dist-dir> <version>
# Run as root inside the container, e.g.
#   docker run --rm -v "$PWD:/src" -w /src fedora:39 bash dev/smoke-linux.sh rpm target/dist 0.1.0
set -euo pipefail

format="${1:?usage: smoke-linux.sh <deb|rpm|tar.gz> <dist-dir> <version>}"
dist="${2:?usage: smoke-linux.sh <deb|rpm|tar.gz> <dist-dir> <version>}"
version="${3:?usage: smoke-linux.sh <deb|rpm|tar.gz> <dist-dir> <version>}"
arch="$(uname -m)"
name="fbterm-${version}-linux-${arch}"
file="$dist/$name.$format"
[[ -f "$file" ]] || { echo "missing $file" >&2; exit 1; }
(cd "$dist" && sha256sum -c "$name.$format.sha256")

check_version() {
  local out
  out="$("$1" --version)"
  [[ "$out" == "fbterm $version" ]] || { echo "$1 --version printed '$out', expected 'fbterm $version'" >&2; exit 1; }
  echo "$out"
}

check_installed() {
  [[ "$(command -v fbterm)" == /usr/bin/fbterm ]] || { echo "fbterm is not /usr/bin/fbterm" >&2; exit 1; }
  check_version fbterm
  grep -qx "Name=featherbit terminal" /usr/share/applications/fbterm.desktop
  grep -qx "Categories=System;TerminalEmulator;" /usr/share/applications/fbterm.desktop
  for s in 32 64 128 256 512; do
    [[ -f "/usr/share/icons/hicolor/${s}x${s}/apps/fbterm.png" ]] || { echo "missing ${s}px icon" >&2; exit 1; }
  done
}

case "$format" in
  deb)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends "$(realpath "$file")"
    check_installed
    apt-get remove -y -qq fbterm
    ;;
  rpm)
    dnf install -y -q --setopt=install_weak_deps=False "$(realpath "$file")"
    check_installed
    dnf remove -y -q fbterm
    ;;
  tar.gz)
    work="$(mktemp -d)"
    tar -C "$work" -xzf "$file"
    check_version "$work/$name/bin/fbterm"
    [[ -f "$work/$name/share/applications/fbterm.desktop" && -f "$work/$name/INSTALL" ]] \
      || { echo "tarball is missing the desktop entry or INSTALL" >&2; exit 1; }
    rm -rf "$work"
    exit 0
    ;;
  *) echo "unknown format '$format'" >&2; exit 1 ;;
esac
if [[ -e /usr/bin/fbterm || -e /usr/share/applications/fbterm.desktop ]]; then
  echo "removal left files behind" >&2
  exit 1
fi
# shellcheck source=/dev/null
echo "smoke ok: $format on $(. /etc/os-release && echo "$PRETTY_NAME") $arch"
