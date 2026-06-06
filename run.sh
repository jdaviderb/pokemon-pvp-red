#!/usr/bin/env bash
# Build (if needed) and run the NES-over-WebRTC server.
# Usage: ./run.sh [path/to/rom.nes]
set -euo pipefail
cd "$(dirname "$0")"
export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig
export LIBRARY_PATH=/opt/homebrew/lib
exec cargo run --release -- "$@"
