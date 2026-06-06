# End-to-end WebRTC tests (headless Chrome)

These drive a real headless Chrome (Puppeteer) against a running server to verify the
full media + input + teardown paths. They are how the implementation was validated.

```sh
# 1. start the server in one terminal
./run.sh

# 2. in another terminal, install puppeteer once and run a test
mkdir -p /tmp/nes-rtc-test && cd /tmp/nes-rtc-test && npm init -y && npm i puppeteer
node ~/pokemon-pvp-red/test/e2e-media.cjs    # video+audio decode
node ~/pokemon-pvp-red/test/e2e-input.cjs    # keyboard -> server
node ~/pokemon-pvp-red/test/e2e-cleanup.cjs  # peer teardown
```

Verified results:
- e2e-media:   ICE connected; video framesDecoded>0 at 256x240; audio bytesReceived>0.
- e2e-input:   server logs `input P1: down/up <Button>` for each key pressed.
- e2e-cleanup: after pc.close(), server `viewers` returns to 0 (writer tasks stop).
