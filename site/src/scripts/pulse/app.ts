// /pulse: Zcash's value pools, read live from Sova state (SIP-7 ZcashBlocks
// at 0x…5A01). Two view calls, no library: latest() to find the newest
// anchored Zcash block, summaries(from, to) for the new ones. Polls.
// Config: data-config on #pulse, overridden by ?rpc=&at=&n=
import { rpc, words, num, u256, calldata } from '../checkout/chain';

type Cfg = { rpc: string; at: string; n: number; pollMs: number };
type Blk = {
  height: number; hash: string; time: number; txCount: number; shieldedTxCount: number;
  actions: number; pools: number[]; deltas: number[];
};

// Selectors (`cast sig`), ZcashBlocks.sol.
const SEL = { latest: '52bfe789', summaries: '296f5550' };
const WORDS = 27; // one Block
const NAMES = ['transparent', 'sprout', 'sapling', 'orchard', 'lockbox', 'ironwood'];
const SHIELDED = [1, 2, 3, 5];
// Row -> pools summed. Σ shielded is Sprout + Sapling + Orchard + Ironwood.
const ROWS: Record<string, number[]> = {
  sapling: [2], orchard: [3], ironwood: [5], transparent: [0], shielded: SHIELDED,
};

const $ = (id: string) => document.getElementById(id)!;
const root = $('pulse');
const q = new URLSearchParams(location.search);
const base = JSON.parse(root.dataset.config || '{}') as Cfg;
const cfg: Cfg = {
  rpc: q.get('rpc') || base.rpc,
  at: q.get('at') || base.at,
  n: Math.min(Math.max(Number(q.get('n') || base.n), 8), 1024),
  pollMs: base.pollMs,
};
const still = matchMedia('(prefers-reduced-motion: reduce)').matches;

// ---- chain ------------------------------------------------------------------

const call = (data: string) => rpc<string>(cfg.rpc, 'eth_call', [{ to: cfg.at, data }, 'latest']);
const i64 = (w: string) => {
  const v = num(w);
  return Number(v >= 1n << 255n ? v - (1n << 256n) : v); // |zat| < 2^53: exact
};

async function latest(): Promise<number> {
  return Number(num(words(await call('0x' + SEL.latest))[0]));
}

async function summaries(from: number, to: number): Promise<Blk[]> {
  const w = words(await call(calldata(SEL.summaries, u256(from), u256(to))));
  const n = Number(num(w[1]));
  const out: Blk[] = [];
  for (let i = 0; i < n; i++) {
    const b = w.slice(2 + i * WORDS, 2 + (i + 1) * WORDS);
    const c = (k: number) => Number(num(b[k]));
    out.push({
      height: c(0), hash: b[1], time: c(2), txCount: c(3), shieldedTxCount: c(4),
      actions: c(7) + c(8) + c(9) + c(10), // sapling spends + outputs, orchard, ironwood
      pools: b.slice(12, 18).map((x) => Number(num(x))),
      deltas: b.slice(18, 24).map(i64),
    });
  }
  return out;
}

// ---- format -----------------------------------------------------------------

const MINUS = '−';
const grp = (s: string) => s.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
/** zat -> "1,234.56" (dp decimals, truncated). */
function zec(zat: number, dp = 2): string {
  const neg = zat < 0;
  const a = Math.abs(Math.round(zat));
  const int = grp(Math.floor(a / 1e8).toString());
  const frac = (a % 1e8).toString().padStart(8, '0').slice(0, dp);
  return (neg ? MINUS : '') + int + (dp ? '.' + frac : '');
}
/** Signed delta, trailing zeros trimmed: "+1.25", "−0.01035", "0". */
function delta(zat: number): string {
  if (zat === 0) return '0';
  const s = zec(Math.abs(zat), 8).replace(/\.?0+$/, '');
  return (zat > 0 ? '+' : MINUS) + s;
}
const sum = (b: Blk, ids: number[], f: 'pools' | 'deltas') => ids.reduce((a, i) => a + b[f][i], 0);
const short = (h: string) => h.slice(0, 8) + '…' + h.slice(-4);
const utc = (t: number) => new Date(t * 1000).toISOString().slice(11, 19) + ' UTC';

// ---- render -----------------------------------------------------------------

let hist: Blk[] = [];
let hover = -1;
const shown = new Map<string, number>(); // element id -> value on screen (for tweens)

function tween(id: string, to: number, fmt: (v: number) => string) {
  const el = $(id);
  const from = shown.get(id);
  shown.set(id, to);
  if (still || from === undefined || from === to) {
    el.textContent = fmt(to);
    return;
  }
  const t0 = performance.now();
  const step = (t: number) => {
    const k = Math.min(1, (t - t0) / 700);
    const e = 1 - Math.pow(1 - k, 3);
    el.textContent = fmt(from + (to - from) * e);
    if (k < 1 && shown.get(id) === to) requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

function spark(svg: Element, vals: number[], at: number) {
  const W = 160, H = 28, P = 3;
  let lo = Math.min(...vals), hi = Math.max(...vals);
  if (hi === lo) { lo -= 1; hi += 1; }
  const x = (i: number) => (vals.length < 2 ? W : (i / (vals.length - 1)) * W);
  const y = (v: number) => H - P - ((v - lo) / (hi - lo)) * (H - 2 * P);
  const pts = vals.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
  const i = at < 0 ? vals.length - 1 : at;
  svg.innerHTML =
    `<polygon class="a" points="0,${H} ${pts} ${W},${H}"/>` +
    `<polyline class="l" points="${pts}"/>` +
    (at >= 0 ? `<line class="x" x1="${x(i)}" x2="${x(i)}" y1="0" y2="${H}"/>` : '') +
    `<circle class="d" cx="${x(i)}" cy="${y(vals[i])}" r="2.5"/>`;
}

function render(fresh = false) {
  if (!hist.length) return;
  const i = hover >= 0 ? hover : hist.length - 1;
  const b = hist[i];
  const first = hist[0];
  const supply = sum(b, [0, 1, 2, 3, 4, 5], 'pools');

  // hero: Σ shielded
  tween('big', sum(b, SHIELDED, 'pools'), (v) => zec(v, 8));
  const d = sum(b, SHIELDED, 'deltas');
  $('big-d').textContent = delta(d);
  $('big-d').dataset.s = String(Math.sign(d));
  const win = sum(b, SHIELDED, 'pools') - sum(first, SHIELDED, 'pools');
  $('big-w').textContent = delta(win);
  $('big-w').dataset.s = String(Math.sign(win));
  $('big-n').textContent = String(i + 1);
  $('big-share').textContent = ((100 * sum(b, SHIELDED, 'pools')) / supply).toFixed(2) + '%';

  // ticker
  $('zh').textContent = '#' + grp(String(b.height));
  $('zhash').textContent = short(b.hash);
  $('zt').textContent = utc(b.time);
  $('tick').dataset.hover = hover >= 0 ? '1' : '';

  // rows
  for (const [key, ids] of Object.entries(ROWS)) {
    const v = sum(b, ids, 'pools');
    tween('v-' + key, v, (x) => zec(x, 2));
    const dd = sum(b, ids, 'deltas');
    const el = $('d-' + key);
    el.textContent = delta(dd);
    el.dataset.s = String(Math.sign(dd));
    $('p-' + key).textContent = ((100 * v) / supply).toFixed(1) + '%';
    ($('b-' + key) as HTMLElement).style.width = ((100 * v) / supply).toFixed(2) + '%';
    spark($('s-' + key), hist.map((h) => sum(h, ids, 'pools')), hover);
    if (fresh && dd !== 0 && !still) {
      const row = $('r-' + key);
      row.classList.remove('hit');
      void row.offsetWidth; // restart the flash
      row.classList.add('hit');
    }
  }
  $('others').textContent =
    `sprout ${zec(b.pools[1])} · lockbox ${zec(b.pools[4])} · supply ${zec(supply)}`;
}

function feedLine(b: Blk): HTMLLIElement {
  const li = document.createElement('li');
  const moves = b.deltas
    .map((dz, i) => (dz ? `<span data-s="${Math.sign(dz)}">${NAMES[i]} ${delta(dz)}</span>` : ''))
    .filter(Boolean)
    .join(' ');
  li.innerHTML =
    `<b>${grp(String(b.height))}</b> ${moves || '<span class="dim">no pool moved</span>'} ` +
    `<span class="dim">${b.txCount} tx · ${b.actions} shielded ${b.actions === 1 ? 'op' : 'ops'}</span>`;
  return li;
}

function renderFeed(newOnes: Blk[]) {
  const ol = $('feed');
  for (const b of newOnes) {
    const li = feedLine(b);
    if (!still && ol.children.length) li.classList.add('in');
    ol.prepend(li);
  }
  while (ol.children.length > 8) ol.lastElementChild!.remove();
}

function status(msg: string, kind: 'wait' | 'ok' | 'err') {
  const el = $('status');
  el.dataset.k = kind;
  $('st').textContent = msg;
}

function pulse() {
  if (still) return;
  const dot = $('dot');
  dot.classList.remove('beat');
  void dot.offsetWidth;
  dot.classList.add('beat');
}

// ---- loop -------------------------------------------------------------------

let busy = false;
async function tick() {
  if (busy) return;
  busy = true;
  try {
    const top = await latest();
    const sova = Number(BigInt(await rpc<string>(cfg.rpc, 'eth_blockNumber')));
    $('sh').textContent = '#' + grp(String(sova));
    if (!top) {
      status('waiting for the first anchored Zcash block', 'wait');
      return;
    }
    const have = hist.length ? hist[hist.length - 1].height : 0;
    if (top < have) hist = hist.filter((b) => b.height <= top); // Sova reorg: roll back
    if (top > have || !hist.length) {
      const from = hist.length ? have + 1 : Math.max(0, top - cfg.n + 1);
      const got = await summaries(from, top);
      const fresh = hist.length > 0;
      hist = hist.concat(got).slice(-cfg.n);
      if (hover >= hist.length) hover = -1;
      render(fresh);
      renderFeed(fresh ? got : got.slice(-8));
      if (fresh) pulse();
    }
    root.dataset.live = '1';
    status(`live · ${hist.length} Zcash blocks from Sova state`, 'ok');
  } catch (e) {
    status(`can't read ${cfg.rpc}: ${e instanceof Error ? e.message : e}`, 'err');
  } finally {
    busy = false;
  }
}

function onHover(ev: PointerEvent) {
  const r = (ev.currentTarget as Element).getBoundingClientRect();
  const k = Math.round(((ev.clientX - r.left) / r.width) * (hist.length - 1));
  const next = Math.max(0, Math.min(hist.length - 1, k));
  if (next !== hover) {
    hover = next === hist.length - 1 ? -1 : next;
    render();
  }
}
for (const el of document.querySelectorAll<SVGElement>('.spark')) {
  el.addEventListener('pointermove', onHover);
  el.addEventListener('pointerleave', () => {
    hover = -1;
    render();
  });
}

$('cfg').textContent = `rpc ${cfg.rpc} · ZcashBlocks ${cfg.at} · last ${cfg.n} blocks`;
tick();
setInterval(tick, cfg.pollMs);
