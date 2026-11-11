#!/bin/sh
# Invoked in a fresh per-enrollment release directory, after checksum verification.
set -eu
chmod 700 agentworks launch
# Fail here for an incompatible CPU, libc or loader, or another worker protocol,
# before switching the host to this release. $1 is the server's protocol.
reply=$(./agentworks --worker-protocol)
case "$reply" in
  *"\"workerProtocol\":${1:-}}"*) ;;
  *) echo "This worker reports $reply; the server needs protocol ${1:-?}." >&2; exit 1 ;;
esac
mkdir -p "$HOME/.local/bin"
# A host-specific command is recorded for reconnects; this is only a convenience.
# Do not replace an existing user command, only a link to an earlier release,
# which a worker update removes.
link="$HOME/.local/bin/agentworks"
if [ ! -e "$link" ] && [ ! -L "$link" ]; then
  ln -s "$PWD/launch" "$link"
elif [ -L "$link" ]; then
  case "$(readlink "$link")" in
    "$HOME/.local/share/agentworks/releases/"*/launch|"$(dirname "$PWD")/"*/launch)
      ln -sfn "$PWD/launch" "$link.new" && mv -f "$link.new" "$link" ;;
  esac
fi
printf '\nWorker installed. Reconnects use its absolute launch path.\n'
