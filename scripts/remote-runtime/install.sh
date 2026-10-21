#!/bin/sh
# Invoked in a fresh per-enrollment release directory, after checksum verification.
set -eu
mise_bin=$(command -v mise || true)
if [ -z "$mise_bin" ]; then mise_bin="$HOME/.local/bin/mise"; fi
if [ ! -x "$mise_bin" ]; then
  echo 'mise is required. Install it on this host: https://mise.jdx.dev/getting-started.html' >&2
  exit 1
fi
"$mise_bin" trust "$PWD/mise.toml"
"$mise_bin" install
"$mise_bin" exec -- npm ci --ignore-scripts --omit=dev --no-audit --no-fund
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
