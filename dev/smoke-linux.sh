#!/usr/bin/env bash
# Installs one Linux package in a clean container and checks it, as the release workflow's
# smoke jobs do: a minimal install (no recommended or weak dependencies), `fbterm --version`,
# the desktop entry and icons, then removal.
# Usage: dev/smoke-linux.sh <deb|rpm|tar.gz> <dist-dir> <version>
# Run as root inside a throwaway container (it installs and removes system packages), e.g.
#   docker run --rm -v "$PWD:/src" -w /src fedora:39 bash dev/smoke-linux.sh rpm target/dist 0.1.0
# It refuses to run anywhere else: outside a container and outside CI.
set -euo pipefail

if [[ ! -e /.dockerenv && ! -e /run/.containerenv && -z "${CI:-}" ]]; then
  echo "smoke-linux.sh installs and removes system packages: run it in a container or in CI" >&2
  exit 1
fi

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

# The libraries fbterm opens at run time (dlopen, so the package manager cannot see them) and
# the package declares as hard dependencies: after a minimal install each must resolve.
# Keep in step with the depends and requires in crates/fbterm/Cargo.toml.
runtime_libs=(libxkbcommon.so.0 libxkbcommon-x11.so.0 libwayland-client.so.0 libX11.so.6
  libX11-xcb.so.1 libXcursor.so.1 libXi.so.6 libxcb.so.1 libdbus-1.so.3)

check_runtime_libs() {
  local cache lib
  cache="$(ldconfig -p 2>/dev/null || /sbin/ldconfig -p)"
  for lib in "${runtime_libs[@]}"; do
    grep -qE "^[[:space:]]+${lib//./\\.} \(" <<<"$cache" \
      || { echo "$lib does not resolve after the install (ldconfig -p)" >&2; exit 1; }
  done
  echo "runtime libraries resolve: ${runtime_libs[*]}"
}

check_installed() {
  [[ "$(command -v fbterm)" == /usr/bin/fbterm ]] || { echo "fbterm is not /usr/bin/fbterm" >&2; exit 1; }
  check_version fbterm
  grep -qx "Name=featherbit terminal" /usr/share/applications/fbterm.desktop
  grep -qx "Categories=System;TerminalEmulator;" /usr/share/applications/fbterm.desktop
  for s in 32 64 128 256 512; do
    [[ -f "/usr/share/icons/hicolor/${s}x${s}/apps/fbterm.png" ]] || { echo "missing ${s}px icon" >&2; exit 1; }
  done
  check_runtime_libs
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
