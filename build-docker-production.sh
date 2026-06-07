#!/usr/bin/env bash
# Build the production Pokemon Red PVP image for linux/amd64 (bundling the ROM, the Linux gambatte core, the
# savestates and the static UI), then print how to run it.
#
# On an arm64 host (Apple Silicon) this builds amd64 via QEMU emulation — correct but slow; on an
# amd64 host / CI it's native and fast.
set -euo pipefail
cd "$(dirname "$0")"

IMAGE="${IMAGE:-pokemon-red-pvp:prod}"
PORT="${PORT:-3000}"

echo "==> Building $IMAGE for linux/amd64 ..."
docker buildx build --platform linux/amd64 -t "$IMAGE" --load .

echo
echo "==> Built $IMAGE (linux/amd64). Run it with:"
echo "    docker run --rm --network host -e DEV=1 $IMAGE   # --network host so WebRTC video works"
echo "    then open http://localhost:3000"
