#!/usr/bin/env bash
# pkg_version <version>: maps a release version to what the .deb and the .rpm can carry, setting
# deb_version and rpm_version. Debian and RPM sort "~" before anything, so a pre-release precedes
# its release; RPM versions may not contain "-". The tilde must come from a variable: "\~" inside
# the expansion stays literal under bash 3.2 (the /bin/bash of macOS), putting a backslash in
# every package version there. Sourced by dev/package-linux.sh, tested by
# dev/test-package-version.sh on both bashes.

# shellcheck disable=SC2034  # deb_version and rpm_version are the outputs: the caller reads them.
pkg_version() {
  local version="$1"
  tilde='~'
  deb_version="${version//-/$tilde}"
  rpm_version="${version//-/$tilde}"
}
