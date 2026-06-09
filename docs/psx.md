# Running a PlayStation game (e.g. Bloody Roar)

The libretro frontend is **core-agnostic**, so it streams a PSX game over WebRTC exactly like it
streams Game Boy — just point it at a PSX core + a disc. Verified with **Bloody Roar (USA, SCUS-94199)**:
boots, the intro FMV streams at **60fps**, stable.

## Setup

1. **Core** — use **`mednafen_psx`** (Beetle PSX, pure software). On Apple Silicon (arm64 macOS) it's
   rock-solid. Grab it from the buildbot:
   ```sh
   cd cores && curl -fsSL -o md.zip \
     https://buildbot.libretro.com/nightly/apple/osx/arm64/latest/mednafen_psx_libretro.dylib.zip \
     && unzip -o md.zip && rm md.zip && xattr -d com.apple.quarantine mednafen_psx_libretro.dylib
   ```
   > **Avoid `pcsx_rearmed` on arm64 macOS.** It has an HLE BIOS (no real BIOS needed) but its
   > dynarec/fastmem can't map memory at its fixed addresses on Apple Silicon (`psxMap: tried to map
   > @80000000, got 0x…` in the log) → random native crashes a few seconds in. It's fine on Linux
   > x86_64 (the prod target), where HLE works without a BIOS.

2. **BIOS** (required by mednafen/swanstation; copyrighted, **not** in the repo) — drop the PSX BIOS
   files into **`./system/`** (gitignored). For a US disc you need `scph5501.bin` (also keep
   `scph5500.bin` JP / `scph5502.bin` EU for other regions). The system dir is the **`SYSTEM_DIR`** env
   (default `./system`); it's auto-created and passed to the core as an absolute path.

3. **Disc** — a `.cue` + its `.bin` track(s) (gitignored). The core opens these itself (`need_fullpath`).

## Run

```sh
DEV=1 cargo run --release -- "Bloody Roar (USA)/Bloody Roar (USA).cue" cores/mednafen_psx_libretro.dylib
```

Then watch it stream (a WebRTC viewer — the TV/room page, or `/console` in DEV). It runs at the game's
native resolution (320×240); the pipeline re-inits the VP8 canvas on PSX resolution changes, and only
encodes while someone is watching.

## Notes

- `/debug/frame`'s PPM dump can crash on a PSX framebuffer — use a real WebRTC viewer instead.
- No code change is needed to switch consoles — the GB-specific `forced_option`s are ignored by other
  cores (a core only queries its own keys). PSX core tweaks live in `forced_option` too.
- **Next — 1v1 PvP:** a fighting game has native 2-player input, so PvP is just routing player A's keys
  to PSX controller port 1 and player B's to port 2 (much simpler than the Pokémon battle engine).
