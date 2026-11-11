#!/bin/sh
# Build a server bundle: this platform's server plus a worker for every platform
# its hosts may run. Host enrollment and Update worker pick the worker that
# matches each host's OS and CPU.
#
#   target/bundle/agentworks-<platform>/
#     agentworks                                    server (and its own worker)
#     workers/<os>-<arch>/agentworks-worker[.exe]
#   target/bundle/agentworks-<platform>.tar.gz
#
# Platform names are `<os>-<arch>` as workers report them: linux-x86_64,
# linux-aarch64, macos-aarch64, windows-x86_64, ...
#
# Usage: scripts/build-bundle.sh [--debug] [--target TRIPLE]...
#   Builds release binaries by default. Each --target cross-builds one more worker,
#   e.g. x86_64-pc-windows-gnu (needs MinGW-w64), aarch64-unknown-linux-gnu
#   (needs an aarch64 linker configured for Cargo). Without --target, the Windows
#   worker is added when x86_64-w64-mingw32-gcc is installed.
#
# The same workers also land in target/<profile>/workers/, so the
# freshly built server finds them when run straight from target/.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo="$ROOT/scripts/mise-cargo.sh"
profile=release
targets=
while [ $# -gt 0 ]; do
  case "$1" in
    --debug) profile=debug ;;
    --target) targets="$targets $2"; shift ;;
    *) echo "build-bundle: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done
if [ -z "$targets" ] && command -v x86_64-w64-mingw32-gcc >/dev/null; then
  targets=x86_64-pc-windows-gnu
fi
flag=; [ "$profile" = release ] && flag=--release

# Rust target triple -> worker platform name.
platform() {
  arch=${1%%-*}
  case "$1" in
    *-linux-*) os=linux ;;
    *-windows-*) os=windows ;;
    *-apple-darwin) os=macos ;;
    *-freebsd) os=freebsd ;;
    *) echo "build-bundle: unsupported target $1" >&2; exit 2 ;;
  esac
  echo "$os-$arch"
}
native=$(rustc -vV | sed -n 's/^host: //p')
native_platform=$(platform "$native")

"$cargo" build --locked $flag -p agentworks -p agentworks-worker
out="$ROOT/target/$profile"
workers="$out/workers"
rm -rf "$workers"
mkdir -p "$workers/$native_platform"
cp "$out/agentworks-worker" "$workers/$native_platform/agentworks-worker"

for target in $targets; do
  [ "$target" = "$native" ] && continue
  p=$(platform "$target")
  exe=agentworks-worker; [ "${p%%-*}" = windows ] && exe=agentworks-worker.exe
  "$cargo" build --locked $flag --target "$target" -p agentworks-worker
  mkdir -p "$workers/$p"
  cp "$ROOT/target/$target/$profile/$exe" "$workers/$p/$exe"
done

name="agentworks-$native_platform"
bundle="$ROOT/target/bundle/$name"
rm -rf "$bundle"
mkdir -p "$bundle"
cp "$out/agentworks" "$bundle/agentworks"
cp -R "$workers" "$bundle/workers"
tar -czf "$ROOT/target/bundle/$name.tar.gz" -C "$ROOT/target/bundle" "$name"
echo "Bundle: target/bundle/$name.tar.gz"
(cd "$bundle/workers" && ls -1 | sed 's/^/  worker: /')
