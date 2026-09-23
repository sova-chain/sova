import random

# 12 wide x 14 tall. Mirror the left 6 cols, EXCEPT pupils: drawn after
# mirroring so both eyes look the same direction (owls stare, never cross).
BASE = [
 "......",  # r0  tufts overlay
 "..DDDD",  # r1  head top
 ".DBBBB",  # r2
 "DEEEBB",  # r3  eye rows: 3x3 eye at x1-3
 "DEEEBB",  # r4
 "DEEEBB",  # r5
 ".DBBBB",  # r6  beak overlay at center
 ".DDBBB",  # r7  chin
 "..DWCC",  # r8  body
 ".DWCCC",  # r9
 ".DWCCC",  # r10
 ".DWCCC",  # r11
 "..DBBB",  # r12
 "......",  # r13 feet overlay
]
TUFTS = ["......", "..D...", ".DD...", "..DD.."]
FEET  = ["...N.N", "..N..N", "...NN."]
def pal(B, D, W, E="#FBF3DC", K="#F4B728", N="#F4B728", bg="#1C1914"):
    return dict(bg=bg, B=B, D=D, W=W, E=E, P=bg, K=K, N=N)

# Gold is the signature: every palette keeps gold beak + feet (N/K),
# except bright (dark beak for contrast) and spectral. Weights = rarity.
PALETTES = {
 "classic":  pal("#B0841D", "#967019", "#6B500F"),
 "bright":   pal("#F4B728", "#B0841D", "#967019", K="#FBF3DC", N="#967019"),
 "bronze":   pal("#967019", "#6B500F", "#4A370B"),
 "barn":     pal("#D8B56A", "#A67C3B", "#8A6430"),
 "moss":     pal("#7B8F3E", "#5C6B2E", "#414D20"),
 "ember":    pal("#C25A33", "#94422B", "#6E2F1E"),
 "dusk":     pal("#7E6BA8", "#5D4E80", "#443A61"),
 "slate":    pal("#7C8792", "#5A636D", "#414951"),
 "snowy":    pal("#E8E4D8", "#B9B4A5", "#8F8B7E", E="#F4B728"),
 "midnight": pal("#3E4A6B", "#2C3550", "#1E2438"),
 "spectral": pal("#FBF3DC", "#B0841D", "#E4D9B8", E="#F4B728", N="#B0841D"),
}
WEIGHTS = [20,12,12,10,10,8,8,8,6,4,2]

def gen(seed:int, forced=None):
    rng = random.Random(seed)
    g = [list(r) for r in BASE]
    g[0] = list(rng.choice(TUFTS))
    g[13] = list(rng.choice(FEET))
    roll = rng.random()
    sleepy = roll < 0.15
    zen = 0.15 <= roll < 0.21  # rare: eyes fully closed
    if sleepy or zen:  # dark eyelid over the top eye row
        for x in (1,2,3): g[3][x] = 'D'
    if zen:  # closed eyes: cream disc with a dark lash line across
        for x in (1,2,3):
            g[3][x] = 'E'; g[4][x] = 'D'; g[5][x] = 'E'
    g[6][5] = 'N'
    if rng.random() < 0.2: g[7][5] = 'N'
    pal_name = forced if forced else rng.choices(list(PALETTES), weights=WEIGHTS)[0]
    palette = PALETTES[pal_name]
    stripes = rng.random() < 0.4
    rows = []
    for y in range(14):
        full = g[y] + [g[y][x] for x in range(5,-1,-1)]
        row = []
        for ch in full:
            if ch == 'C':
                ch = ('B' if (y % 2 == 0) else 'D') if stripes else rng.choices('BDW', weights=[65,25,10])[0]
            row.append(ch)
        rows.append(row)
    # Pupils AFTER mirroring: same dx for both eyes -> a glance, never a cross-eye.
    if not zen:
        dx = rng.choices([-1,0,1], weights=[15,70,15])[0]
        tall = (rng.random() < 0.35) and not sleepy
        for cx in (2, 9):  # eye centers
            rows[4][cx+dx] = 'P'
            if tall:
                rows[5][cx+dx] = 'P'
    return rows, palette, pal_name

def svg(rows, pal):
    cells = []
    for y,row in enumerate(rows):
        for x,ch in enumerate(row):
            if ch == '.': continue
            cells.append(f'<rect x="{x}" y="{y}" width="1" height="1" fill="{pal[ch]}"/>')
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 14" shape-rendering="crispEdges">'
            f'<rect width="12" height="14" fill="{pal["bg"]}"/>' + "".join(cells) + '</svg>')

names = list(PALETTES)
for i in range(12):
    forced = names[i] if i < len(names) else None
    rows, p, name = gen(i * 104729 + 57, forced)
    open(f"owl7-{i}.svg","w").write(svg(rows, p))
    print(i, name)
