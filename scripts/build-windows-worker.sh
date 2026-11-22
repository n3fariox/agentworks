#!/bin/sh
# Cross-build the Windows worker and place it beside the server binary as
# agentworks-worker.exe, where host enrollment looks for it. Needs a MinGW-w64
# toolchain (x86_64-w64-mingw32-gcc). Pass --release for the release profile.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
profile=debug
for arg; do [ "$arg" = --release ] && profile=release; done
"$ROOT/scripts/mise-cargo.sh" build --locked --target x86_64-pc-windows-gnu -p agentworks-worker "$@"
cp "$ROOT/target/x86_64-pc-windows-gnu/$profile/agentworks-worker.exe" "$ROOT/target/$profile/agentworks-worker.exe"
# The bundle layout, which enrollment prefers (see scripts/build-bundle.sh).
mkdir -p "$ROOT/target/$profile/workers/windows-x86_64"
cp "$ROOT/target/x86_64-pc-windows-gnu/$profile/agentworks-worker.exe" "$ROOT/target/$profile/workers/windows-x86_64/"
echo "Windows worker: target/$profile/workers/windows-x86_64/agentworks-worker.exe"
