#!/usr/bin/env bash
# Tests pkg_version (dev/lib/pkg-version.sh), the mapping dev/package-linux.sh feeds to cargo-deb
# and cargo-generate-rpm: a pre-release gets "~" separators, which Debian and RPM sort before
# anything (so it precedes its release) and which RPM allows where "-" is banned; a plain version
# is unchanged. Must pass under any bash, 3.2 (the /bin/bash of macOS) through 5.x: the tilde
# comes from a variable because "\~" inside the expansion stays literal under 3.2. The release
# workflow runs it beside dev/test-release-version.sh.
# Usage: bash dev/test-package-version.sh   (any bash, any OS)
set -euo pipefail

# shellcheck source=dev/lib/pkg-version.sh
source "$(cd "$(dirname "$0")" && pwd)/lib/pkg-version.sh"
fail() { echo "FAIL: $*" >&2; exit 1; }

check() { # <version> <expected package version>
  pkg_version "$1"
  [[ "$deb_version" == "$2" ]] || fail "'$1' gave deb_version '$deb_version', expected '$2'"
  [[ "$rpm_version" == "$2" ]] || fail "'$1' gave rpm_version '$rpm_version', expected '$2'"
  echo "ok: $1 -> $2"
}

check 1.2.0-rc.1 '1.2.0~rc.1'
check 0.11.0 '0.11.0'
check 1.0.0-beta-2 '1.0.0~beta~2'

echo "pkg-version.sh: all checks passed"
