#!/usr/bin/env bash
# Wait until the bastion's sshd answers with an SSH banner on 127.0.0.1:2222.
# A published port accepts connections before sshd is up, so check for the banner itself.
set -uo pipefail
for _ in $(seq 1 90); do
  banner=""
  if exec 3<>/dev/tcp/127.0.0.1/2222 2>/dev/null; then
    read -r -t 2 -n 4 banner <&3 || true
    exec 3>&- 3<&-
  fi
  if [ "$banner" = "SSH-" ]; then
    echo "sshd ready on 127.0.0.1:2222"
    exit 0
  fi
  sleep 1
done
echo "sshd on 127.0.0.1:2222 did not come up" >&2
exit 1
