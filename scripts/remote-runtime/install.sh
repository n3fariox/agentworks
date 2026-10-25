#!/bin/sh
# Invoked in a fresh per-enrollment release directory, after checksum verification.
set -eu
chmod 700 agentworks launch
# Fail here for incompatible libc/loaders, before saving this host.
./agentworks --service-protocol
mkdir -p "$HOME/.local/bin"
# A host-specific command is recorded for reconnects; this is only a convenience.
# Do not replace an existing user command.
if [ ! -e "$HOME/.local/bin/agentworks" ] && [ ! -L "$HOME/.local/bin/agentworks" ]; then
  ln -s "$PWD/launch" "$HOME/.local/bin/agentworks"
fi
printf '\nWorker installed. Reconnects use its absolute launch path.\n'
