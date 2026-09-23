#!/usr/bin/env python3
"""Ashwings 24x24 (PFP / punks-grade) design lab generator.

This is the art spec: contracts/src/Ashwings.sol is a byte-for-byte port of
it (golden files: --fixtures; check: design/check-parity.sh). The old 12x14
generator (owl-generator.py) is kept only as history.

Construction (every step is integer-only and cheap to port):

  1. HALF: a 24-row x 12-col symbol template (the owl's left half, as seen by
     the viewer). Half-space overlays are stamped onto it: eye state (lids),
     chest pattern mask (symmetric patterns only).
  2. MIRROR: full[y][x] = half[y][x] for x < 12, half[y][23 - x] for x >= 12.
  3. POST-MIRROR overlays, in this fixed order (these are what break the
     symmetry): tufts (left/right sprites, 'wild' differs per side), speckle
     chest (per-cell seed bits), beak, pupils (same dx for both eyes: owls
     cannot cross their eyes), wink (closes the viewer-right eye), void-iris
     catchlight (one cream pixel per open eye, on the glance side), burn
     embers (background cells only), accessory sprite.
  4. RESOLVE: symbol -> hex color via species palette + fixed accessory
     colors + background trait.
  5. SVG: one full-canvas <rect> for the background, then per row run-length
     rects of equal color (runs equal to the background color are skipped).

Seed -> traits (seed is the 32-byte keccak value the contract stores):
  byte b of the seed = uint8(seed[b]) in Solidity.
  Every trait uses ONE byte and a weight table that sums to exactly 256, so
  roll = uint8(seed[b]) is unbiased and "pick the first i with
  roll < cum[i]" is the whole algorithm. Speckle uses the low 140 bits of
  uint256(seed) (2 bits per chest cell), which never overlap bytes 0..9.

  byte 0 species | 1 background | 2 tufts | 3 eye state | 4 glance
  byte 5 pupil   | 6 iris       | 7 beak  | 8 chest     | 9 accessory

CLI:
  ashwing24.py --seed HEX [--svg out.svg] [--png out.png --scale 20]
  ashwing24.py --sheet out.png --count 48 [--cols 6] [--scale 7]
  ashwing24.py --stats [--n 10000]
  ashwing24.py --heroes DIR    (7 hero singles at 480px + a 48px avatar strip)
  ashwing24.py --fixtures DIR  (parity golden files for the forge test)

Sample seeds (sheet/stats): seed_i = keccak256(abi.encodePacked(uint256(i))),
i = 1..N — uniform 256-bit values, exactly like on-chain seeds.
"""
import argparse, os, sys, statistics

W = H = 24
HALF_W = 12

# ---------------------------------------------------------------------------
# Symbols
#   .  background        O  outline / pupil (warm black)
#   B  body              D  rim / shade        W  wing
#   L  facial disc       E  eye (iris)          N  beak (gold)
#   C  chest zone (resolved by the chest trait before colour lookup)
#   K  gold accent       Y  deep-gold accent    R  red   r  dark red
#   T  pipe wood         Q  ember (lit bowl)    S  smoke
#   H  cream highlight (halo glint, void-iris catchlight)
#   G  halo gold (K, or Y on the gold background)
# ---------------------------------------------------------------------------

HALF = [
    "............",  # r0   tufts / halo
    "............",  # r1
    "............",  # r2
    "............",  # r3
    "......OOOOOO",  # r4   crown outline
    "....OODDDDDD",  # r5   crown rim (v4's top D row)
    "...ODBBBBBBD",  # r6   centre ridge D at x11
    "...ODBLLLLBD",  # r7   facial disc lobe top
    "...ODLLEELLD",  # r8   eye 4x4 at x6..9, rows 8..11, corners rounded
    "...ODLEEEELL",  # r9   ridge stops a row above the beak (spectral: D == N)
    "...ODLEEEELB",  # r10  beak overlay at x11
    "...ODLLEELLB",  # r11
    "...ODLLLLLLB",  # r12  lobes merge under the beak: heart-shaped face
    "...ODBLLLLLL",  # r13
    "...ODBBBLLLL",  # r14
    "....ODBBBBBB",  # r15  chin
    "...OWDBBBBBB",  # r16  shoulders: folded wing W
    "..OWWWDCCCCC",  # r17  chest zone x7..11 (full 7..16), rows 17..23
    "..OWDWDCCCCC",  # r18
    ".OWWWWDCCCCC",  # r19
    ".OWDWWDCCCCC",  # r20
    ".OWWWWDCCCCC",  # r21
    ".OWWDWDCCCCC",  # r22
    ".OWWWWDCCCCC",  # r23  (bust cropped by the canvas edge)
]
EYE_X0, EYE_Y0 = 6, 8          # left eye box (4x4); right eye box at x 14..17
EYE_R_X0 = 23 - EYE_X0 - 3     # 14
CHEST_X0, CHEST_X1 = 7, 16     # inclusive, full coords
CHEST_Y0, CHEST_Y1 = 17, 23

# ---------------------------------------------------------------------------
# Palettes. Species keep v4's B/D/W and gold beak; L (facial disc) is new.
# ---------------------------------------------------------------------------
OUTLINE = "#1C1914"
SPECIES = [
    # name,      B,         D,         W,         L,         E,         N
    ("classic",  "#B0841D", "#967019", "#6B500F", "#CDA24A", "#FBF3DC", "#F4B728"),
    ("bright",   "#F4B728", "#B0841D", "#967019", "#F9D57A", "#FBF3DC", "#967019"),
    ("bronze",   "#967019", "#6B500F", "#4A370B", "#B38C3A", "#FBF3DC", "#F4B728"),
    ("barn",     "#D8B56A", "#A67C3B", "#8A6430", "#F4E9CF", "#FBF3DC", "#F4B728"),
    ("moss",     "#7B8F3E", "#5C6B2E", "#414D20", "#9CAD5E", "#FBF3DC", "#F4B728"),
    ("ember",    "#C25A33", "#94422B", "#6E2F1E", "#DB8058", "#FBF3DC", "#F4B728"),
    ("dusk",     "#7E6BA8", "#5D4E80", "#443A61", "#A08FC4", "#FBF3DC", "#F4B728"),
    ("slate",    "#7C8792", "#5A636D", "#414951", "#A0AAB3", "#FBF3DC", "#F4B728"),
    ("snowy",    "#E8E4D8", "#B9B4A5", "#8F8B7E", "#FAF8F1", "#F4B728", "#F4B728"),
    ("midnight", "#3E4A6B", "#2C3550", "#1E2438", "#56648A", "#FBF3DC", "#F4B728"),
    ("spectral", "#FBF3DC", "#B0841D", "#E4D9B8", "#FFFDF6", "#F4B728", "#B0841D"),
]
# Weights out of 256 (v4's 20/12/12/10/10/8/8/8/6/4/2 percent, rescaled).
SPECIES_W = [51, 31, 31, 26, 26, 20, 20, 20, 15, 11, 5]

BACKGROUNDS = [  # name, colour
    ("ash",    "#3D362E"),
    ("slate",  "#5B6F7E"),
    ("sage",   "#6E7C63"),
    ("cream",  "#FBF3DC"),
    ("clay",   "#94644A"),
    ("night",  "#1C1914"),
    ("gold",   "#F4B728"),
    ("burn",   "#1C1914"),  # night + ember specks
]
BACKGROUND_W = [60, 50, 40, 36, 30, 22, 12, 6]

TUFTS = ["none", "tufts", "horns", "tall", "crest", "wild"]
TUFTS_W = [70, 70, 50, 36, 22, 8]

EYES = ["open", "sleepy", "zen", "wink"]
EYES_W = [186, 38, 18, 14]

GLANCE = [-1, 0, 1]
GLANCE_W = [38, 180, 38]

PUPIL = ["round", "tall"]
PUPIL_W = [166, 90]

IRIS = [("natural", None), ("amber", "#E8892B"), ("ember", "#D5452B"), ("void", "#3A3129")]
IRIS_W = [180, 44, 22, 10]

BEAK = ["small", "long", "hooked", "hoot"]
BEAK_W = [128, 80, 30, 18]

CHEST = ["speckle", "bars", "chevrons", "bib"]
CHEST_W = [100, 76, 50, 30]

ACCESSORY = ["none", "gold chain", "headband", "bow tie", "monocle", "earring", "pipe", "halo"]
ACCESSORY_W = [120, 28, 22, 22, 20, 20, 16, 8]

FIXED = {
    "O": OUTLINE,
    "K": "#F4B728", "Y": "#B0841D",
    "R": "#B8322A", "r": "#7E1F1A",
    "T": "#6B4428", "Q": "#F07A2A", "S": "#B9B2A6",
    "H": "#FBF3DC",  # cream highlight: halo glint, void-iris catchlight
}

for w in (SPECIES_W, BACKGROUND_W, TUFTS_W, EYES_W, GLANCE_W, PUPIL_W, IRIS_W,
          BEAK_W, CHEST_W, ACCESSORY_W):
    assert sum(w) == 256, w
assert len(SPECIES_W) == len(SPECIES) and len(BACKGROUND_W) == len(BACKGROUNDS)

# ---------------------------------------------------------------------------
# Sprites. Full-canvas coordinates; '.' = transparent (leave cell as is).
# Tuft sprites are drawn for the LEFT side (x 0..11) and mirrored for the
# right, except 'wild' which uses a different right-hand sprite.
# ---------------------------------------------------------------------------
TUFT_SPRITES = {  # rows 0..5, cols 0..11
    "none": [],
    "tufts": [
        "............",
        "............",
        "....O.......",
        "....ODO.....",
        "....ODDO....",
        ".....D......",
    ],
    "horns": [
        "..O.........",
        "..OO........",
        "..ODO.......",
        "..ODDO......",
        "...ODDO.....",
        ".....D......",
    ],
    "tall": [
        ".....O......",
        "....ODO.....",
        "....ODO.....",
        "....ODDO....",
        "....ODDO....",
        ".....DD.....",
    ],
    "crest": [
        "............",
        "...........O",
        "........O.OD",
        "........ODOD",
        ".........DDD",
        "............",
    ],
}
WILD_RIGHT = [  # a flopped tuft, drawn in LEFT-half coords then mirrored
    "............",
    "............",
    "............",
    "..OOO.......",
    "...ODDO.....",
    ".....D......",
]

def sprite(rows, y0=0):
    """rows of strings -> list of (x, y, sym) for non-'.' cells."""
    out = []
    for dy, row in enumerate(rows):
        for x, ch in enumerate(row):
            if ch != ".":
                out.append((x, y0 + dy, ch))
    return out

def mirror_cells(cells):
    return [(23 - x, y, s) for (x, y, s) in cells]

ACCESSORY_SPRITES = {
    "none": [],
    # gold chain: U of gold links with dark gaps (reads on gold species
    # too) and a rimmed coin pendant
    "gold chain": [
        (6, 16, "K"), (7, 17, "O"), (8, 17, "K"), (9, 18, "O"), (10, 18, "K"),
        (13, 18, "K"), (14, 18, "O"), (15, 17, "K"), (16, 17, "O"), (17, 16, "K"),
        (11, 18, "O"), (12, 18, "O"),
        (10, 19, "O"), (11, 19, "K"), (12, 19, "K"), (13, 19, "O"),
        (10, 20, "O"), (11, 20, "Y"), (12, 20, "Y"), (13, 20, "O"),
        (11, 21, "O"), (12, 21, "O"),
    ],
    # headband across the crown, knot + tails trailing off the owl's right
    "headband": [(x, 6, "R") for x in range(4, 20)] + [(x, 5, "r") for x in range(6, 18)] + [
        (3, 6, "R"), (2, 6, "r"), (2, 7, "R"), (1, 8, "R"), (2, 8, "r"), (1, 9, "r"),
    ],
    # bow tie just under the chin: two wings tapering into a dark knot
    "bow tie": [
        (8, 15, "R"), (15, 15, "R"),
        (8, 16, "R"), (9, 16, "R"), (10, 16, "R"), (11, 16, "r"), (12, 16, "r"), (13, 16, "R"), (14, 16, "R"), (15, 16, "R"),
        (8, 17, "R"), (15, 17, "R"),
        (9, 15, "R"), (14, 15, "R"), (9, 17, "R"), (14, 17, "R"),
    ],
    # dark-rimmed monocle around the viewer-right eye (a gold rim vanishes on
    # the gold species), gold glint, chain of gold/dark links to the wing
    "monocle": [(x, 7, "O") for x in range(14, 18)] + [(x, 12, "O") for x in range(14, 18)] + [
        (13, 8, "O"), (13, 9, "O"), (13, 10, "O"), (13, 11, "O"),
        (18, 8, "O"), (18, 9, "O"), (18, 10, "O"), (18, 11, "O"),
        (17, 7, "K"),
        (18, 12, "K"), (19, 13, "O"), (19, 14, "K"), (20, 15, "O"), (20, 16, "K"),
    ],
    # gold hoop on the owl's right side of the head
    "earring": [(2, 13, "K"), (1, 14, "K"), (3, 14, "K"), (2, 15, "K")],
    # pipe from the beak, bowl hanging to the viewer-right, a wisp of smoke
    "pipe": [
        (13, 12, "T"), (14, 13, "T"), (15, 13, "T"),
        (16, 12, "Q"), (17, 12, "Q"),
        (16, 13, "T"), (17, 13, "T"), (16, 14, "T"), (17, 14, "T"),
    ],
    # floating gold halo: a hollow 1px elliptical ring, 10 wide (x 7..16),
    # rows 0..2, cream glints on the top edge. Its hollow (row 1) and the
    # air under it (rows 2..3, x 8..15) are cleared to background first, so
    # the background always shows through the middle and a full row of air
    # separates ring and crown (this flattens a crest to its base). 'G' is
    # halo gold: gold, or deep gold on the gold background (see colors()).
    "halo": [(x, 1, ".") for x in range(9, 15)] + [(x, y, ".") for y in (2, 3) for x in range(8, 16)] + [
        (9, 0, "G"), (10, 0, "H"), (11, 0, "H"), (12, 0, "G"), (13, 0, "G"), (14, 0, "G"),
        (7, 1, "G"), (8, 1, "G"), (15, 1, "G"), (16, 1, "G"),
        (9, 2, "G"), (10, 2, "G"), (11, 2, "G"), (12, 2, "G"), (13, 2, "G"), (14, 2, "G"),
    ],
}

# Burn background: fixed ember specks, drawn only on background cells.
EMBERS = [(1, 2, "Q"), (21, 1, "K"), (19, 4, "Q"), (2, 11, "K"), (22, 8, "Q"),
          (0, 17, "Q"), (23, 13, "K"), (4, 1, "K"), (22, 20, "K"), (1, 21, "K")]

# Chevron chest mask (half coords x 7..11, rows 17..23): 'D' marks.
CHEVRON_HALF = [
    ".....",
    "D....",
    ".D..D",
    "..DD.",
    "D....",
    ".D..D",
    "..DD.",
]

# ---------------------------------------------------------------------------
# Seed / traits
# ---------------------------------------------------------------------------
def pick(roll, weights):
    acc = 0
    for i, w in enumerate(weights):
        acc += w
        if roll < acc:
            return i
    raise AssertionError

def traits(seed: bytes):
    assert len(seed) == 32
    t = {}
    t["species"] = pick(seed[0], SPECIES_W)
    t["background"] = pick(seed[1], BACKGROUND_W)
    t["tufts"] = pick(seed[2], TUFTS_W)
    t["eyes"] = pick(seed[3], EYES_W)
    t["dx"] = GLANCE[pick(seed[4], GLANCE_W)]
    t["pupil"] = pick(seed[5], PUPIL_W)
    t["iris"] = pick(seed[6], IRIS_W)
    t["beak"] = pick(seed[7], BEAK_W)
    t["chest"] = pick(seed[8], CHEST_W)
    t["accessory"] = pick(seed[9], ACCESSORY_W)
    return t

def eyes_label(t):
    st = EYES[t["eyes"]]
    if st == "zen":
        return "zen"
    if st in ("sleepy", "wink"):
        return st
    if t["dx"] != 0:
        return "glance left" if t["dx"] < 0 else "glance right"
    return "wide stare" if PUPIL[t["pupil"]] == "tall" else "stare"

def attributes(t):
    return {
        "species": SPECIES[t["species"]][0],
        "background": BACKGROUNDS[t["background"]][0],
        "tufts": TUFTS[t["tufts"]],
        "eyes": eyes_label(t),
        "iris": IRIS[t["iris"]][0],
        "beak": BEAK[t["beak"]],
        "chest": CHEST[t["chest"]],
        "accessory": ACCESSORY[t["accessory"]],
    }

# ---------------------------------------------------------------------------
# Grid
# ---------------------------------------------------------------------------
def _put(g, cells):
    for x, y, s in cells:
        if 0 <= x < W and 0 <= y < H:
            g[y][x] = s

def _close_eye(g, x0):
    """Closed eye in a 4x4 box at (x0, 8): lid fill + a smiling lash curve."""
    for yy in range(8, 12):
        for xx in range(x0, x0 + 4):
            if g[yy][xx] == "E" or g[yy][xx] == "O":
                g[yy][xx] = "D"
    for xx in range(x0 + 1, x0 + 3):
        g[8][xx] = "L"  # keep the round eye outline readable
    g[9][x0] = "O"; g[9][x0 + 3] = "O"
    g[10][x0 + 1] = "O"; g[10][x0 + 2] = "O"

def grid(seed: bytes):
    t = traits(seed)
    half = [list(r) for r in HALF]
    eyes = EYES[t["eyes"]]

    # --- half-space: symmetric eye states --------------------------------
    if eyes == "sleepy":
        for xx in range(EYE_X0 + 1, EYE_X0 + 3):
            half[8][xx] = "D"
        for xx in range(EYE_X0, EYE_X0 + 4):
            half[9][xx] = "D"

    # --- half-space: symmetric chest patterns ----------------------------
    chest = CHEST[t["chest"]]
    for y in range(CHEST_Y0, CHEST_Y1 + 1):
        for x in range(CHEST_X0, HALF_W):
            if half[y][x] != "C":
                continue
            if chest == "bars":
                half[y][x] = "D" if y % 2 == 0 else "B"
            elif chest == "chevrons":
                half[y][x] = CHEVRON_HALF[y - CHEST_Y0][x - CHEST_X0].replace(".", "B")
            elif chest == "bib":  # light V-necked breast, widening downward
                half[y][x] = "L" if x >= max(8, 10 - (y - CHEST_Y0)) else "B"
            # speckle resolved post-mirror (asymmetric)

    # --- mirror ----------------------------------------------------------
    g = [[half[y][x if x < HALF_W else 23 - x] for x in range(W)] for y in range(H)]

    # --- tufts -----------------------------------------------------------
    tuft = TUFTS[t["tufts"]]
    if tuft == "wild":
        _put(g, sprite(TUFT_SPRITES["horns"]))
        _put(g, mirror_cells(sprite(WILD_RIGHT)))
    elif tuft != "none":
        cells = sprite(TUFT_SPRITES[tuft])
        _put(g, cells)
        _put(g, mirror_cells(cells))

    # --- speckle: 2 bits per chest cell from the low bits of the seed ----
    if chest == "speckle":
        s = int.from_bytes(seed, "big")
        for y in range(CHEST_Y0, CHEST_Y1 + 1):
            for x in range(CHEST_X0, CHEST_X1 + 1):
                i = (y - CHEST_Y0) * 10 + (x - CHEST_X0)
                v = (s >> (2 * i)) & 3
                # staggered spot lattice on even rows; each spot: 1/4 none,
                # 1/2 shade, 1/4 wing-dark
                spot = y % 2 == 0 and (x + y // 2) % 2 == 0
                g[y][x] = ("B", "D", "D", "W")[v] if spot else "B"

    # --- beak (centre columns 11, 12) -------------------------------------
    beak = BEAK[t["beak"]]
    for x in (11, 12):
        g[10][x] = "N"; g[11][x] = "N"
        if beak in ("long", "hooked"):
            g[12][x] = "N"
        if beak == "hoot":
            g[11][x] = "O"; g[12][x] = "N"
    if beak == "hooked":
        g[13][11] = "N"  # tip curls to one side

    # --- pupils: same dx for both eyes (never cross-eyed) -----------------
    if eyes in ("open", "sleepy", "wink"):
        dx = t["dx"]
        tall = PUPIL[t["pupil"]] == "tall" and eyes != "sleepy"
        rows = (10,) if eyes == "sleepy" else ((9, 10, 11) if tall else (9, 10))
        for x0 in (EYE_X0, EYE_R_X0):
            for yy in rows:
                for xx in (x0 + 1 + dx, x0 + 2 + dx):
                    g[yy][xx] = "O"
    if eyes == "zen":
        _close_eye(g, EYE_X0)
        _close_eye(g, EYE_R_X0)
    if eyes == "wink":
        _close_eye(g, EYE_R_X0)

    # --- void iris catchlight: the dark iris swallows the dark pupil, so a
    # single cream pixel on the pupil's top row marks where the owl looks:
    # the pupil's left cell for a left glance or a stare (a conventional
    # upper-left catchlight), its right cell for a right glance. Open eyes
    # only (the winked eye stays shut). -------------------------------------
    if IRIS[t["iris"]][0] == "void" and eyes in ("open", "sleepy", "wink"):
        dx = t["dx"]
        yy = 10 if eyes == "sleepy" else 9
        for x0 in ((EYE_X0,) if eyes == "wink" else (EYE_X0, EYE_R_X0)):
            g[yy][x0 + 1 + dx if dx <= 0 else x0 + 2 + dx] = "H"

    # --- burn background embers -------------------------------------------
    if BACKGROUNDS[t["background"]][0] == "burn":
        for x, y, s in EMBERS:
            if g[y][x] == ".":
                g[y][x] = s

    # --- accessory --------------------------------------------------------
    _put(g, ACCESSORY_SPRITES[ACCESSORY[t["accessory"]]])
    return g, t

def colors(t):
    name, B, D, Wc, L, E, N = SPECIES[t["species"]]
    iris = IRIS[t["iris"]][1]
    pal = dict(FIXED)
    pal.update(B=B, D=D, W=Wc, L=L, E=iris or E, N=N)
    pal["."] = BACKGROUNDS[t["background"]][1]
    # halo gold: a gold ring would vanish on the gold background
    pal["G"] = pal["Y"] if BACKGROUNDS[t["background"]][0] == "gold" else pal["K"]
    return pal

def pixels(seed: bytes):
    g, t = grid(seed)
    pal = colors(t)
    return [[pal[ch] for ch in row] for row in g], t, pal["."]

# ---------------------------------------------------------------------------
# SVG (row run-length rects)
# ---------------------------------------------------------------------------
def runs(px, bg):
    out = []
    for y, row in enumerate(px):
        x = 0
        while x < W:
            c = row[x]; x1 = x
            while x1 + 1 < W and row[x1 + 1] == c:
                x1 += 1
            if c != bg:
                out.append((x, y, x1 - x + 1, c))
            x = x1 + 1
    return out

def svg(seed: bytes):
    px, t, bg = pixels(seed)
    r = runs(px, bg)
    body = "".join(f'<rect x="{x}" y="{y}" width="{w}" height="1" fill="{c}"/>' for x, y, w, c in r)
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" shape-rendering="crispEdges">'
            f'<rect width="24" height="24" fill="{bg}"/>' + body + "</svg>"), len(r) + 1

# ---------------------------------------------------------------------------
# Seeds
# ---------------------------------------------------------------------------
def keccak(b: bytes) -> bytes:
    from Crypto.Hash import keccak as _k
    h = _k.new(digest_bits=256); h.update(b); return h.digest()

def sample_seed(i: int) -> bytes:
    return keccak(i.to_bytes(32, "big"))

def parse_seed(s: str) -> bytes:
    s = s[2:] if s.lower().startswith("0x") else s
    b = bytes.fromhex(s)
    if len(b) != 32:
        b = keccak(b)  # short/odd input: hash it into a full seed
    return b

# ---------------------------------------------------------------------------
# Raster output
# ---------------------------------------------------------------------------
def image(seed: bytes, scale: int):
    from PIL import Image
    px, _, _ = pixels(seed)
    im = Image.new("RGB", (W, H))
    im.putdata([tuple(int(c[i:i + 2], 16) for i in (1, 3, 5)) for row in px for c in row])
    return im.resize((W * scale, H * scale), Image.NEAREST)

def _font(size):
    from PIL import ImageFont
    for p in ("/System/Library/Fonts/SFNSMono.ttf", "/System/Library/Fonts/Menlo.ttc"):
        try:
            return ImageFont.truetype(p, size)
        except OSError:
            pass
    return ImageFont.load_default(size=size)

def short_label(t):
    a = attributes(t)
    l1 = f'{a["species"]} · {a["background"]}'
    l2 = a["eyes"] + (f' · {a["iris"]} iris' if a["iris"] != "natural" else "")
    l3 = f'{a["tufts"]} tufts · {a["beak"]} beak' if a["tufts"] not in ("tufts",) else f'tufts · {a["beak"]} beak'
    l3 = l3.replace("none tufts", "no tufts")
    l4 = a["chest"] + (f' · {a["accessory"]}' if a["accessory"] != "none" else "")
    return l1, l2, l3, l4

def sheet(seeds, out, cols=6, scale=7, labels=True):
    from PIL import Image, ImageDraw
    cell = W * scale
    pad, lab = 16, (66 if labels else 0)
    rows = (len(seeds) + cols - 1) // cols
    im = Image.new("RGB", (cols * (cell + pad) + pad, rows * (cell + pad + lab) + pad), (0x14, 0x12, 0x0F))
    d = ImageDraw.Draw(im)
    f = _font(11)
    for k, (tag, seed) in enumerate(seeds):
        cx = pad + (k % cols) * (cell + pad)
        cy = pad + (k // cols) * (cell + pad + lab)
        im.paste(image(seed, scale), (cx, cy))
        if labels:
            _, t = grid(seed)
            l1, l2, l3, l4 = short_label(t)
            d.text((cx, cy + cell + 4), f"{tag}  {l1}", fill=(0xF4, 0xB7, 0x28), font=f)
            d.text((cx, cy + cell + 18), l2, fill=(0xE8, 0xE0, 0xCC), font=f)
            d.text((cx, cy + cell + 32), l3, fill=(0xB9, 0xB2, 0xA6), font=f)
            d.text((cx, cy + cell + 46), l4, fill=(0xB9, 0xB2, 0xA6), font=f)
    im.save(out, optimize=True)
    return im.size

# ---------------------------------------------------------------------------
# Stats
# ---------------------------------------------------------------------------
def stats(n):
    from collections import Counter
    cnt = {k: Counter() for k in ("species", "background", "tufts", "eyes", "iris", "beak", "chest", "accessory")}
    rects, combos = [], Counter()
    for i in range(1, n + 1):
        seed = sample_seed(i)
        s, r = svg(seed)
        rects.append(r)
        _, t = grid(seed)
        a = attributes(t)
        for k in cnt:
            cnt[k][a[k]] += 1
        combos[tuple(a.values())] += 1
    tables = {
        "species": (SPECIES_W, [s[0] for s in SPECIES]),
        "background": (BACKGROUND_W, [b[0] for b in BACKGROUNDS]),
        "tufts": (TUFTS_W, TUFTS),
        "iris": (IRIS_W, [i[0] for i in IRIS]),
        "beak": (BEAK_W, BEAK),
        "chest": (CHEST_W, CHEST),
        "accessory": (ACCESSORY_W, ACCESSORY),
    }
    print(f"# {n} seeds (seed_i = keccak256(uint256(i)), i=1..{n})\n")
    for k, c in cnt.items():
        print(f"## {k}")
        print("| value | weight/256 | expected | observed |")
        print("|---|---|---|---|")
        if k in tables:
            ws, names = tables[k]
            for w, nm in zip(ws, names):
                print(f"| {nm} | {w} | {100*w/256:.2f}% | {100*c[nm]/n:.2f}% |")
        else:
            for nm, v in c.most_common():
                print(f"| {nm} | derived | | {100*v/n:.2f}% |")
        print()
    rs = sorted(rects)
    print("## rects per SVG (incl. background rect)")
    print(f"min {rs[0]}  median {statistics.median(rs)}  mean {statistics.mean(rs):.1f}  "
          f"p99 {rs[int(0.99*len(rs))-1]}  max {rs[-1]}")
    print(f"unique full trait combos: {len(combos)} / {n}  (max repeats {max(combos.values())})")
    # theoretical worst case over the design: every pixel its own rect = 576 + 1

# ---------------------------------------------------------------------------
def find_seed(pred, start=1, limit=200000):
    for i in range(start, start + limit):
        s = sample_seed(i)
        if pred(attributes(traits(s))):
            return i, s
    raise SystemExit("no seed found")

def showcase(out, scale=7):
    """One real sample seed per trait value (review aid, not a rarity sample)."""
    wants = []
    for k, names in (("species", [s[0] for s in SPECIES]), ("background", [b[0] for b in BACKGROUNDS]),
                     ("tufts", TUFTS), ("eyes", ["stare", "wide stare", "glance left", "glance right", "sleepy", "zen", "wink"]),
                     ("iris", [i[0] for i in IRIS]), ("beak", BEAK), ("chest", CHEST), ("accessory", ACCESSORY)):
        for v in names:
            wants.append((k, v))
    seeds = []
    for k, v in wants:
        i, s = find_seed(lambda a: a[k] == v)
        seeds.append((f"#{i}", s))
    return sheet(seeds, out, cols=8, scale=scale)

def heroes(outdir):
    from PIL import Image
    os.makedirs(outdir, exist_ok=True)
    wants = [
        ("hero-1", lambda a: a["species"] == "classic" and a["accessory"] == "gold chain" and a["tufts"] == "horns" and a["eyes"] == "stare"),
        ("hero-2", lambda a: a["species"] == "barn" and a["accessory"] == "monocle" and a["background"] == "slate"),
        ("hero-3", lambda a: a["species"] == "snowy" and a["accessory"] == "headband" and a["eyes"].startswith("glance")),
        ("hero-4", lambda a: a["species"] == "midnight" and a["accessory"] == "pipe" and a["background"] == "cream"),
        ("hero-5", lambda a: a["species"] == "ember" and a["eyes"] == "wink" and a["accessory"] == "bow tie"),
        ("hero-6", lambda a: a["species"] == "spectral" and a["accessory"] == "halo" and a["background"] == "burn"),
        ("hero-7", lambda a: a["iris"] == "void" and a["eyes"].startswith("glance") and a["accessory"] == "none"),
    ]
    found = []
    for name, pred in wants:
        i, s = find_seed(pred)
        image(s, 20).save(os.path.join(outdir, f"{name}.png"), optimize=True)
        open(os.path.join(outdir, f"{name}.svg"), "w").write(svg(s)[0])
        found.append((name, i, s))
        print(name, f"i={i}", "0x" + s.hex(), attributes(traits(s)))
    # 48px avatar strip: every hero at native 2x (incl. the halo and a
    # void-iris owl), on dark and light UI strips
    picks = [f[2] for f in found]
    pad = 16
    strip = Image.new("RGB", (pad + len(picks) * (48 + pad), 2 * (48 + pad) + pad), (0x1C, 0x19, 0x14))
    light = Image.new("RGB", (strip.width, 48 + 2 * pad - pad // 2), (0xF8, 0xF9, 0xFA))
    strip.paste(light, (0, 48 + pad + pad // 2))
    for k, s in enumerate(picks):
        a = image(s, 2)
        strip.paste(a, (pad + k * (48 + pad), pad))
        strip.paste(a, (pad + k * (48 + pad), 48 + 2 * pad + pad // 2))
    strip.save(os.path.join(outdir, "avatar-strip-48px.png"), optimize=True)
    return found

# ---------------------------------------------------------------------------
# Parity fixtures (golden files for contracts/test/AshwingsParity.t.sol)
# ---------------------------------------------------------------------------
TRAIT_TABLES = [SPECIES_W, BACKGROUND_W, TUFTS_W, EYES_W, GLANCE_W, PUPIL_W,
                IRIS_W, BEAK_W, CHEST_W, ACCESSORY_W]

# Worst case found by coordinate-ascent search over every trait byte and
# speckle pattern (234 rects incl. the background rect; the 20k sample
# seeds top out at 231).
WORST_SEED = bytes.fromhex("0000be002600f6d000aa000000000fffffffffffffffffffffffffffffffffff")

def attributes_json(t):
    return "[" + ",".join('{"trait_type":"%s","value":"%s"}' % kv for kv in attributes(t).items()) + "]"

def parity_seeds():
    """(label, seed) pairs: sample seeds 1..48, the all-zero / all-ones
    seeds, one real sample seed per trait value (searched from i=1000 so
    they are new seeds), void iris with every eye state, halo combos, a
    seed per weight-table boundary byte (roll = cum-1 and cum in all ten
    trait bytes at once), the heaviest sample seed and the searched worst
    case. Duplicates are dropped."""
    out = [(f"sample-{i}", sample_seed(i)) for i in range(1, 49)]
    out += [("zero", bytes(32)), ("max", b"\xff" * 32)]
    wants = []
    for k, names in (("species", [s[0] for s in SPECIES]), ("background", [b[0] for b in BACKGROUNDS]),
                     ("tufts", TUFTS), ("eyes", ["stare", "wide stare", "glance left", "glance right", "sleepy", "zen", "wink"]),
                     ("iris", [i[0] for i in IRIS]), ("beak", BEAK), ("chest", CHEST), ("accessory", ACCESSORY)):
        wants += [(f"{k}={v}", (lambda k, v: lambda a: a[k] == v)(k, v)) for v in names]
    for e in ("stare", "wide stare", "glance left", "glance right", "sleepy", "zen", "wink"):
        wants.append((f"void+{e}", (lambda e: lambda a: a["iris"] == "void" and a["eyes"] == e)(e)))
    wants.append(("halo+void", lambda a: a["accessory"] == "halo" and a["iris"] == "void"))
    wants.append(("halo+crest", lambda a: a["accessory"] == "halo" and a["tufts"] == "crest"))
    wants.append(("halo+burn", lambda a: a["accessory"] == "halo" and a["background"] == "burn"))
    for label, pred in wants:
        i, s = find_seed(pred, start=1000, limit=2_000_000)
        out.append((f"{label} (i={i})", s))
    bounds = sorted({c for w in TRAIT_TABLES for c in (sum(w[:j]) for j in range(1, len(w)))})
    for c in bounds:
        for v in (c - 1, c):
            b = bytearray(keccak(bytes([v])))
            b[0:10] = bytes([v]) * 10
            out.append((f"bytes0-9=0x{v:02x}", bytes(b)))
    heavy = max(range(1, 20001), key=lambda i: svg(sample_seed(i))[1])
    out.append((f"heaviest sample (i={heavy})", sample_seed(heavy)))
    out.append(("worst case (searched)", WORST_SEED))
    seen, uniq = set(), []
    for label, s in out:
        if s not in seen:
            seen.add(s); uniq.append((label, s))
    return uniq

def write_fixtures(outdir):
    """seeds.txt / attrs.txt / svgs.txt / labels.txt, one line per seed, in
    lockstep (the forge test reads them with vm.readLine, no FFI)."""
    os.makedirs(outdir, exist_ok=True)
    lines = {"seeds": [], "attrs": [], "svgs": [], "labels": []}
    rows = parity_seeds()
    for label, s in rows:
        t = traits(s)
        body, n = svg(s)
        lines["seeds"].append("0x" + s.hex())
        lines["attrs"].append(attributes_json(t))
        lines["svgs"].append(body)
        lines["labels"].append(f"{label}\trects={n}")
    for k, v in lines.items():
        with open(os.path.join(outdir, f"{k}.txt"), "w") as f:
            f.write("\n".join(v) + "\n")
    return len(rows)

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seed", help="32-byte hex seed (0x optional); other lengths are keccak-hashed")
    ap.add_argument("--svg", help="write the seed's SVG here")
    ap.add_argument("--png", help="write the seed's PNG here")
    ap.add_argument("--scale", type=int, default=None, help="PNG/sheet pixel scale")
    ap.add_argument("--sheet", help="contact sheet PNG path")
    ap.add_argument("--count", type=int, default=48)
    ap.add_argument("--start", type=int, default=1, help="first sample index for --sheet")
    ap.add_argument("--cols", type=int, default=6)
    ap.add_argument("--stats", action="store_true")
    ap.add_argument("--n", type=int, default=10000)
    ap.add_argument("--heroes", help="dir for hero singles + avatar strip")
    ap.add_argument("--showcase", help="sheet with one sample per trait value")
    ap.add_argument("--grid", action="store_true", help="print the symbol grid for --seed")
    ap.add_argument("--fixtures", help="dir for the Solidity parity golden files")
    a = ap.parse_args()
    did = False
    if a.fixtures:
        print(f"fixtures: {write_fixtures(a.fixtures)} seeds -> {a.fixtures}"); did = True
    if a.seed:
        seed = parse_seed(a.seed)
        s, r = svg(seed)
        print("seed 0x" + seed.hex())
        print(attributes(traits(seed)), f"rects={r}")
        if a.grid:
            g, _ = grid(seed)
            print("\n".join("".join(row) for row in g))
        if a.svg:
            open(a.svg, "w").write(s)
        if a.png:
            image(seed, a.scale or 20).save(a.png)
        did = True
    if a.sheet:
        seeds = [(f"#{i}", sample_seed(i)) for i in range(a.start, a.start + a.count)]
        size = sheet(seeds, a.sheet, cols=a.cols, scale=a.scale or 7)
        print(f"sheet {a.sheet} {size[0]}x{size[1]} ({os.path.getsize(a.sheet)//1024} KB)")
        did = True
    if a.showcase:
        print("showcase", showcase(a.showcase, scale=a.scale or 7)); did = True
    if a.heroes:
        heroes(a.heroes); did = True
    if a.stats:
        stats(a.n); did = True
    if not did:
        ap.print_help()

if __name__ == "__main__":
    main()
