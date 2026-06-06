#!/usr/bin/env bash
# Download the prebuilt arm64 macOS libretro N64 cores (software/angrylion path).
set -euo pipefail
cd "$(dirname "$0")"
BB=https://buildbot.libretro.com/nightly/apple/osx/arm64/latest
for c in gambatte sameboy parallel_n64 mupen64plus_next; do
  echo "fetching $c ..."
  curl -fsSL -o "$c.zip" "$BB/${c}_libretro.dylib.zip"
  unzip -o "$c.zip" >/dev/null && rm -f "$c.zip"
done
xattr -d com.apple.quarantine ./*.dylib 2>/dev/null || true
echo "done: $(ls *.dylib)"
