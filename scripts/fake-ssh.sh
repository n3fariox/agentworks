#!/usr/bin/env bash
# Test stand-in for ssh (AGENTWORKS_SSH). Accepts the arguments Agentworks
# passes and runs the remote command on this machine with stdin/stdout passed
# through, as ssh does. Destinations starting with "unreachable" fail the way
# ssh does; destinations starting with "noisy" print shell startup chatter on
# stdout first, as some remote .bashrc files do.
set -euo pipefail
while [ $# -gt 0 ]; do
  case "$1" in
    -T) shift ;;
    -o) shift 2 ;;
    --) shift; break ;;
    -*) echo "fake-ssh: unexpected option $1" >&2; exit 255 ;;
    *) break ;;
  esac
done
destination=$1; shift
case "$destination" in
  unreachable*) echo "ssh: connect to host $destination port 22: Connection refused" >&2; exit 255 ;;
  noisy*) printf 'Welcome to %s\nLast login: yesterday\n' "$destination" ;;
esac
exec bash -c "$*"
