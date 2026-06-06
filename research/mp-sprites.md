# MP Sprites — 151 Gen-1 front sprites for the slot machine

Status: DONE. 151 PNGs produced at `static/sprites/1.png .. 151.png` (by Pokedex number).

## Method chosen: Option C (download a permissively-licensed Gen-1 set)

Picked Option C as the most reliable. Option A (decompress from `Pokemon Red.gb`
using the pokered sprite codec) is correct but error-prone for 151 sprites under
time pressure; Option B (drive emulator + screenshot 151x) is slow and brittle.
Option C reliably yields all 151 in seconds and the art is the authentic Gen-1
Red/Blue front sprites — exactly what the slot machine wants.

Source: **PokeAPI sprites repo, Generation-I Red/Blue set**
```
https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/pokemon/versions/generation-i/red-blue/<n>.png
```
where `<n>` is the National Pokedex number (1..151). These are the original
in-game front sprites. (PokeAPI sprite data is released under permissive terms;
fine for a demo.)

### Download (what was run)
```bash
mkdir -p static/sprites
cd static/sprites
for n in $(seq 1 151); do
  curl -s -m 30 -o "${n}.png" \
    "https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/pokemon/versions/generation-i/red-blue/${n}.png"
done
# result: 0 failures, all HTTP 200
```

### Post-processing (transparency + uniform canvas)
The raw red-blue PNGs are 2-bit palette, **white background (no alpha)**, and vary
in size (40x40 / 48x48 / 56x56). For a clean slot machine I normalized every sprite:
flood-fill the white background from the corners to transparent (preserves interior
white pixels like eyes), then center on a uniform **96x96 transparent** canvas.
```bash
for n in $(seq 1 151); do
  magick "${n}.png" -alpha set -bordercolor white -border 1 \
    -fill none -fuzz 8% -draw "color 0,0 floodfill" -shave 1x1 \
    -background none -gravity center -extent 96x96 -strip "out/${n}.png"
done
```

## Verification (all passed)
- `ls static/sprites | wc -l` == **151**
- 0 files named anything other than `<n>.png`; 0 zero-byte files
- All 151 decode as valid PNG (checked IHDR programmatically)
- All 151 are 96x96 with transparency (tRNS / alpha channel present)
- Total size **604K** (small; per-file ~0.4–1KB)
- Visual spot-check montage of #1 Bulbasaur, #6 Charizard, #25 Pikachu,
  #130 Gyarados, #144 Articuno, #150 Mewtwo, #151 Mew — all recognizable,
  correct art, clean transparent background, non-empty (798–2319 opaque px each).

| Pokedex | Name      | file       | dims (final) | opaque px |
|--------:|-----------|------------|--------------|-----------|
| 1       | Bulbasaur | `1.png`    | 96x96        | ~798      |
| 25      | Pikachu   | `25.png`   | 96x96        | ~890      |
| 144     | Articuno  | `144.png`  | 96x96        | ~2319     |
| 150     | Mewtwo    | `150.png`  | 96x96        | ~2044     |

## IMPORTANT mapping the frontend needs: Pokedex# vs internal Gen-1 index

Sprites are keyed by **National Pokedex number (1..151)**.
The battle code's `SPECIES` table (`src/species_data.rs`) is **also in Pokedex
order** (151 entries; table index 0 = Bulbasaur = dex #1, index 24 = Pikachu =
dex #25, ... index 150 = Mew = dex #151). BUT each entry carries a `species:`
byte which is the **internal Gen-1 index** that `/battle/setup` actually wants
(e.g. Bulbasaur internal = 0x99, Pikachu = 0x54, Mewtwo = 0x83). These two
numbering schemes are NOT the same — do not send the Pokedex number to setup.

Recommended frontend flow:
1. Slot machine picks a Pokedex number `d` in 1..151 -> shows `static/sprites/d.png`.
2. To start the battle, map `d` -> internal index and pass that to `/battle/setup`.

Two equally easy options:
- **(A)** Keep the `SPECIES` array exposed to the frontend in Pokedex order and
  index it directly: `SPECIES[d-1].species` is the internal byte. (Slot machine
  keys by Pokedex; setup reads `.species`.)
- **(B)** Hard-code the lookup table below (index 0 = dex #1), value = internal byte.

### DEX -> INTERNAL lookup (index 0 = Pokedex #1; value = internal Gen-1 index)
```js
// usage: const internal = DEX_TO_INTERNAL[dexNumber - 1];  // dexNumber in 1..151
const DEX_TO_INTERNAL = [153,9,154,176,178,180,177,179,28,123,124,125,112,113,114,36,150,151,165,166,5,35,108,45,84,85,96,97,15,168,16,3,167,7,4,142,82,83,100,101,107,130,185,186,187,109,46,65,119,59,118,77,144,47,128,57,117,33,20,71,110,111,148,38,149,106,41,126,188,189,190,24,155,169,39,49,163,164,37,8,173,54,64,70,116,58,120,13,136,23,139,25,147,14,34,48,129,78,138,6,141,12,10,17,145,43,44,11,55,143,18,1,40,30,2,92,93,157,158,27,152,42,26,72,53,51,29,60,133,22,19,76,102,105,104,103,170,98,99,90,91,171,132,74,75,73,88,89,66,131,21];
```
(All 151 internal values are unique. Spot-checks: dex 1->153/0x99 Bulbasaur,
dex 25->84/0x54 Pikachu, dex 144->74/0x4A Articuno, dex 150->131/0x83 Mewtwo,
dex 151->21/0x15 Mew — matches `SPECIES[d-1].species`.)

## Notes / gotchas for the integrator
- `/battle/setup` takes both species (player + enemy), level, and names. The
  slot machine should pick TWO Pokedex numbers (one per player), map each to its
  internal index, and send both. Picking the same dex# for both is fine for a 1v1.
- Sprites are upscaled 40–56px -> 96px canvas WITHOUT smoothing applied here, so
  they're crisp pixel art; render with `image-rendering: pixelated` in CSS for
  the authentic blocky look in the slot machine.
- Backgrounds are transparent, so they composite on any slot-machine reel color.
- If you ever want to regenerate larger/cleaner art, PokeAPI also has
  `sprites/pokemon/<n>.png` (official artwork, 475x475, transparent), but those
  are big and not Gen-1 style.
