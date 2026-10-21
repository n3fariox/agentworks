#!/bin/sh
# Run Cargo with this project's pinned native build toolchain.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

MISE_BIN=${MISE_BIN:-$(command -v mise || true)}
if [ -z "$MISE_BIN" ] || [ ! -x "$MISE_BIN" ]; then
  echo 'mise is required to install and select the pinned Zig toolchain. See https://mise.jdx.dev/getting-started.html' >&2
  exit 1
fi

"$MISE_BIN" install
exec "$MISE_BIN" exec -- cargo "$@"
