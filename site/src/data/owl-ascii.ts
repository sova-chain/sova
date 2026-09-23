// Turns an on-chain Ashwing SVG (24x24 grid: one full-canvas background
// rect, then per-row run-length rects of height 1, rendered by
// contracts/src/Ashwings.sol; samples in contracts/samples/) into monochrome
// ASCII art at build time. Each owl's colours are ranked by brightness and
// mapped onto a glyph ramp, so body, disc, beak and eyes stay distinct. The
// owl's background colour and the warm-black outline/pupils (#1C1914)
// become spaces. Each pixel is two characters wide so it reads roughly square.
// Returns HTML: runs of the same rank are wrapped in <span class="rN">.
import fs from 'node:fs';
import path from 'node:path';

const N = 24;
const OUTLINE = '#1c1914';
const RAMP = [':', '=', '+', '*', '#', '%', '@']; // dark -> light

function lum(hex: string) {
  const n = parseInt(hex.slice(1), 16);
  const r = (n >> 16) & 255, g = (n >> 8) & 255, b = n & 255;
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function owlAscii(file: string): string {
  const svg = fs.readFileSync(path.join(process.cwd(), 'public', 'ashwings', file), 'utf8');
  const grid: (string | null)[][] = Array.from({ length: N }, () => Array(N).fill(null));
  // Run rects only (the background rect has no x/y, so it never matches).
  const re = /<rect x="(\d+)" y="(\d+)" width="(\d+)" height="1" fill="(#[0-9A-Fa-f]{6})"\/>/g;
  for (const m of svg.matchAll(re)) {
    const fill = m[4].toLowerCase();
    if (fill === OUTLINE) continue;
    const x = Number(m[1]), y = Number(m[2]), w = Number(m[3]);
    for (let i = x; i < x + w; i++) grid[y][i] = fill;
  }
  const colours = [...new Set(grid.flat().filter(Boolean) as string[])].sort((a, b) => lum(a) - lum(b));
  const rankOf = new Map(colours.map((c, i) => [c, Math.round((i / Math.max(1, colours.length - 1)) * (RAMP.length - 1))]));
  return grid
    .map((row) => {
      let html = '', cur = -1;
      for (const c of row) {
        const r = c ? rankOf.get(c)! : -1;
        if (r !== cur) {
          if (cur >= 0) html += '</span>';
          if (r >= 0) html += `<span class="r${r}">`;
          cur = r;
        }
        html += r >= 0 ? RAMP[r] + RAMP[r] : '  ';
      }
      if (cur >= 0) html += '</span>';
      return html.replace(/\s+$/, '');
    })
    .join('\n');
}
