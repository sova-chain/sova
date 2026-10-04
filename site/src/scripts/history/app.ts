// /history: Sova's history on NEAR, read live in the browser. No server of
// ours sits between the page and the archive: the index contract
// (sova-da.testnet, docs/design/near-da.md §5) is read straight from public
// NEAR testnet RPCs, and "check" downloads a batch's transaction from NEAR and
// hashes it here. The one Sova call is eth_blockNumber/eth_chainId on the
// public RPC, for the head (and so the finality gap).
// Config: data-config on #hist, overridden by ?contract=&near=<url,url>&rpc=
import { rpc } from '../checkout/chain';

type Cfg = {
  contract: string; near: string[]; rpc: string; explorer: string; nearExplorer: string;
  page: number; pollMs: number;
};
type Info = {
  format: string; owner: string; chain_id: number; start_height: number; next_height: number;
  batch_count: number; last_hash: string | null; bytes_posted: number;
};
type Batch = {
  index: number; first_height: number; last_height: number; count: number; bytes: number;
  sha256: string; last_hash: string; near_block: number; tx_hash: string | null;
};

const $ = (id: string) => document.getElementById(id)!;
const root = $('hist');
const q = new URLSearchParams(location.search);
const base = JSON.parse(root.dataset.config || '{}') as Cfg;
const ACCOUNT = /^(?=.{2,64}$)(([a-z\d]+[-_])*[a-z\d]+\.)*([a-z\d]+[-_])*[a-z\d]+$/;
const okUrl = (u: string) => /^https:\/\//.test(u) || /^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?(\/|$)/.test(u);
const cfg: Cfg = {
  ...base,
  contract: (q.get('contract') || base.contract).trim().toLowerCase(),
  near: q.get('near') ? q.get('near')!.split(',').map((s) => s.trim()).filter(okUrl) : base.near,
  rpc: q.get('rpc') && okUrl(q.get('rpc')!) ? q.get('rpc')! : base.rpc,
};
const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
const CELLS = 64;

// ---- format -----------------------------------------------------------------

const grp = (n: number) => String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
const host = (u: string) => {
  try { return new URL(u).host; } catch { return u; }
};
function size(b: number): string {
  if (b < 1000) return `${b} B`;
  if (b < 1e6) return `${(b / 1e3).toFixed(b < 1e4 ? 1 : 0)} KB`;
  return `${(b / 1e6).toFixed(2)} MB`;
}
function ago(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 90) return `${s} s ago`;
  if (s < 5400) return `${Math.round(s / 60)} min ago`;
  if (s < 172800) return `${(s / 3600).toFixed(1)} h ago`;
  return `${Math.round(s / 86400)} days ago`;
}
const utc = (ms: number) => new Date(ms).toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
const shortTx = (h: string) => h.slice(0, 6) + '…' + h.slice(-4);
const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

// ---- NEAR -------------------------------------------------------------------

/** An answer from the contract or chain that another RPC won't change. */
class Definitive extends Error {}

let preferred = 0; // index into cfg.near of the last RPC that answered
let lastNearHost = '';

/** One NEAR JSON-RPC call, trying each RPC in turn (last good one first). */
async function near<T = any>(method: string, params: object): Promise<T> {
  const order = cfg.near.map((_, i) => (preferred + i) % cfg.near.length);
  const errs: string[] = [];
  for (const i of order) {
    const url = cfg.near[i];
    try {
      const res = await fetch(url, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 'sova', method, params }),
        signal: AbortSignal.timeout(12_000),
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const j = await res.json();
      if (j.error) {
        const name: string = j.error.cause?.name || j.error.name || 'error';
        if (name === 'UNKNOWN_ACCOUNT' || name === 'NO_CONTRACT_CODE') {
          throw new Definitive(name === 'UNKNOWN_ACCOUNT' ? `${cfg.contract}: no such NEAR account` : `${cfg.contract}: no contract deployed`);
        }
        throw new Error(name);
      }
      if (j.result?.error) throw new Definitive(`${cfg.contract}: ${j.result.error}`);
      preferred = i;
      lastNearHost = host(url);
      return j.result as T;
    } catch (e) {
      if (e instanceof Definitive) throw e;
      const m = e instanceof DOMException && e.name === 'TimeoutError' ? 'timeout' : errText(e);
      errs.push(`${host(url)}: ${m === 'Failed to fetch' ? 'unreachable' : m}`);
    }
  }
  throw new Error(errs.join('; ') || 'no NEAR RPC configured');
}

async function view<T>(method: string, args: object): Promise<T> {
  const r = await near<{ result: number[] }>('query', {
    request_type: 'call_function',
    finality: 'final',
    account_id: cfg.contract,
    method_name: method,
    args_base64: btoa(JSON.stringify(args)),
  });
  return JSON.parse(new TextDecoder().decode(new Uint8Array(r.result))) as T;
}

async function batches(from: number, n: number): Promise<Batch[]> {
  const out: Batch[] = [];
  while (n > 0) {
    const page = await view<Batch[]>('batches', { from_index: from, limit: Math.min(n, 100) });
    if (!page.length) break;
    out.push(...page);
    from += page.length;
    n -= page.length;
  }
  return out;
}

const blockTimes = new Map<number, number>();
async function nearBlockTime(h: number): Promise<number> {
  if (!blockTimes.has(h)) {
    const b = await near<{ header: { timestamp_nanosec: string } }>('block', { block_id: h });
    blockTimes.set(h, Number(BigInt(b.header.timestamp_nanosec) / 1_000_000n));
  }
  return blockTimes.get(h)!;
}

/** Download the batch from its NEAR transaction and check it against the index. */
async function check(b: Batch): Promise<string> {
  if (!b.tx_hash) throw new Error('no tx hash recorded yet');
  const r = await near<any>('tx', { tx_hash: b.tx_hash, sender_account_id: info!.owner, wait_until: 'NONE' });
  const act = (r.transaction?.actions || []).find((a: any) => a?.FunctionCall?.method_name === 'post');
  if (!act) throw new Error('transaction has no post call');
  const raw = Uint8Array.from(atob(act.FunctionCall.args), (c) => c.charCodeAt(0));
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', raw));
  const hex = [...digest].map((x) => x.toString(16).padStart(2, '0')).join('');
  if (hex !== b.sha256) throw new Error(`sha256 ${hex.slice(0, 12)}… does not match the index`);
  const dv = new DataView(raw.buffer);
  const magic = new TextDecoder().decode(raw.slice(0, 7));
  const chain = Number(dv.getBigUint64(8, true));
  const first = Number(dv.getBigUint64(16, true));
  const count = dv.getUint32(24, true);
  if (magic !== 'SOVADA1' || raw[7] !== 0) throw new Error('not a SOVADA1 batch');
  if (chain !== info!.chain_id || first !== b.first_height || count !== b.count) {
    throw new Error(`header says chain ${chain}, blocks ${first}+${count}: not what the index says`);
  }
  return `ok: ${grp(raw.length)} bytes from NEAR (${lastNearHost}), sha256 matches the index, SOVADA1 chain ${chain}, blocks ${grp(first)}–${grp(first + count - 1)}`;
}

// ---- state ------------------------------------------------------------------

let info: Info | null = null;
let head: number | null = null;
let sovaChain: number | null = null;
let lastBatchMs = 0;
let shownCount = -1; // batch_count the list reflects
let oldest = 0; // lowest index in the list
const st = { near: 'wait', sova: 'wait', nearMsg: '', sovaMsg: '' };

const thru = () => (info && info.batch_count > 0 ? info.next_height - 1 : null);
const sameChain = () => info !== null && sovaChain !== null && info.chain_id === sovaChain;

// ---- render -----------------------------------------------------------------

const shown = new Map<string, number>();
function roll(id: string, to: number) {
  const el = $(id);
  const from = shown.get(id);
  shown.set(id, to);
  if (still || from === undefined || from === to || Math.abs(to - from) > 5000) {
    el.textContent = '#' + grp(to);
    return;
  }
  const t0 = performance.now();
  const step = (t: number) => {
    const k = Math.min(1, (t - t0) / 600);
    el.textContent = '#' + grp(Math.round(from + (to - from) * (1 - Math.pow(1 - k, 3))));
    if (k < 1 && shown.get(id) === to) requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
  el.classList.remove('bump');
  void el.offsetWidth;
  el.classList.add('bump');
}

const rail = $('rail');
for (let i = 0; i < CELLS; i++) {
  const c = document.createElement('i');
  c.style.setProperty('--i', String(i));
  rail.appendChild(c);
}

function drawRail() {
  const t = thru();
  const cells = rail.children;
  const top = sameChain() && head !== null ? head : t;
  if (top === null) {
    for (const c of cells) c.className = '';
    return;
  }
  const gap = t === null ? 0 : top - t;
  const span = Math.max(Math.ceil((gap * 1.6) / CELLS), Math.ceil(480 / CELLS), 1);
  const lo = top - span * CELLS + 1;
  for (let i = 0; i < CELLS; i++) {
    const a = lo + i * span, z = a + span - 1;
    const k = t !== null && z <= t ? 'on' : t !== null && a <= t ? 'part' : 'wait';
    cells[i].className = i === CELLS - 1 && sameChain() ? `${k} head` : k;
  }
}

function drawNums() {
  const t = thru();
  if (head !== null) roll('head', head);
  if (t !== null) roll('thru', t);
  else $('thru').textContent = info ? 'none yet' : '…';
  const gap = $('gap');
  const why = $('why');
  if (info && sovaChain !== null && !sameChain()) {
    gap.textContent = '—';
    why.textContent = `# this archive is chain ${info.chain_id}; the Sova RPC is chain ${sovaChain}, so there is no gap to show.`;
    why.dataset.k = 'warn';
  } else {
    gap.textContent = t !== null && head !== null ? `${grp(Math.max(0, head - t))} blocks` : '—';
    why.textContent = '# the gap is finality: a block is posted once it’s final on Sova, about two hours after it’s mined.';
    why.dataset.k = '';
  }
  if (!info) return;
  $('chain').textContent = String(info.chain_id);
  $('count').textContent = grp(info.batch_count);
  $('bytes').textContent = info.batch_count ? `${size(info.bytes_posted)} · ${grp(info.next_height - info.start_height)} blocks` : '—';
  $('last').textContent = lastBatchMs ? `${ago(Date.now() - lastBatchMs)} · ${utc(lastBatchMs)}` : info.batch_count ? '…' : 'none yet';
  root.dataset.state = info.batch_count ? 'live' : 'starting';
}

function row(b: Batch): HTMLElement {
  const el = document.createElement('div');
  el.className = 'row';
  el.setAttribute('role', 'row');
  el.dataset.index = String(b.index);
  const blk = (h: number) => `<a href="${esc(cfg.explorer)}/block/${h}">${grp(h)}</a>`;
  const tx = b.tx_hash
    ? `<a href="${esc(cfg.nearExplorer)}/txns/${esc(b.tx_hash)}" title="${esc(b.tx_hash)}">${esc(shortTx(b.tx_hash))}</a>`
    : `<a href="${esc(cfg.nearExplorer)}/blocks/${b.near_block}" class="dim" title="tx hash not recorded yet">block ${grp(b.near_block)}</a>`;
  el.innerHTML =
    `<span role="cell" class="ix">${b.index}</span>` +
    `<span role="cell" class="rg">${blk(b.first_height)}${b.count > 1 ? `–${blk(b.last_height)}` : ''}</span>` +
    `<span role="cell" class="n cnt">${grp(b.count)}</span>` +
    `<span role="cell" class="n sz">${size(b.bytes)}</span>` +
    `<span role="cell" class="sha" title="sha256 ${esc(b.sha256)}"><code>${esc(b.sha256.slice(0, 12))}</code></span>` +
    `<span role="cell" class="tx">${tx}</span>` +
    `<span role="cell" class="ck">${b.tx_hash ? '<button type="button" class="link">check</button>' : ''}</span>` +
    `<span class="res" hidden></span>`;
  const btn = el.querySelector('button');
  btn?.addEventListener('click', async () => {
    const res = el.querySelector<HTMLElement>('.res')!;
    btn.disabled = true;
    res.hidden = false;
    res.dataset.k = 'wait';
    res.textContent = `downloading ${size(b.bytes)} from NEAR…`;
    try {
      res.textContent = await check(b);
      res.dataset.k = 'ok';
    } catch (e) {
      res.textContent = errText(e);
      res.dataset.k = 'err';
    }
    btn.disabled = false;
  });
  return el;
}

function addRows(list: Batch[], where: 'top' | 'bottom', animate: boolean) {
  const rows = $('rows');
  const sorted = [...list].sort((a, b) => b.index - a.index);
  const frag = document.createDocumentFragment();
  sorted.forEach((b, i) => {
    const el = row(b);
    if (animate && !still) {
      el.classList.add('in');
      el.style.animationDelay = `${Math.min(i, 20) * 35}ms`;
    }
    frag.appendChild(el);
  });
  if (where === 'top') rows.prepend(frag);
  else rows.appendChild(frag);
  if (list.length) oldest = Math.min(oldest, ...list.map((b) => b.index));
  $('more').hidden = oldest <= 0;
  $('empty').hidden = rows.children.length > 0;
}

function drawStatus() {
  const s = $('status');
  const err = st.near === 'err' || st.sova === 'err';
  s.dataset.k = err ? 'err' : st.near === 'ok' ? 'ok' : 'wait';
  $('st-near').textContent = st.nearMsg;
  $('st-sova').textContent = st.sovaMsg;
  $('retry').hidden = !err;
}

// ---- loop -------------------------------------------------------------------

async function readNear(first: boolean) {
  const i = await view<Info>('info', {});
  const grew = info !== null && i.batch_count > shownCount;
  // A shrunk index (a new contract) or a big jump (catch-up): show the newest page afresh.
  const reset = info !== null && (i.batch_count < shownCount || i.chain_id !== info.chain_id || i.batch_count - shownCount > cfg.page);
  info = i;
  if (first || reset) {
    $('rows').innerHTML = '';
    oldest = i.batch_count;
    const from = Math.max(0, i.batch_count - cfg.page);
    addRows(await batches(from, i.batch_count - from), 'bottom', true);
    shownCount = i.batch_count;
  } else if (grew) {
    addRows(await batches(shownCount, i.batch_count - shownCount), 'top', true);
    shownCount = i.batch_count;
  }
  if (i.batch_count === 0) {
    $('empty').hidden = false;
    lastBatchMs = 0;
  } else if (first || grew || reset || !lastBatchMs) {
    const [last] = await batches(i.batch_count - 1, 1);
    if (last) lastBatchMs = await nearBlockTime(last.near_block).catch(() => 0);
  }
  st.near = 'ok';
  st.nearMsg = `near: read ${cfg.contract} at final from ${lastNearHost}`;
}

async function readSova() {
  if (sovaChain === null) sovaChain = Number(await rpc<string>(cfg.rpc, 'eth_chainId'));
  head = Number(await rpc<string>(cfg.rpc, 'eth_blockNumber'));
  st.sova = 'ok';
  st.sovaMsg = `sova: head from ${host(cfg.rpc)}`;
}

let busy = false;
let firstNear = true;
let timer = 0;
let nextAt = 0;

async function tick() {
  if (busy) return;
  busy = true;
  clearTimeout(timer);
  const [n, s] = await Promise.allSettled([readNear(firstNear), readSova()]);
  if (n.status === 'fulfilled') firstNear = false;
  else {
    st.near = 'err';
    st.nearMsg = `near: ${errText(n.reason)}`;
  }
  if (s.status === 'rejected') {
    st.sova = 'err';
    st.sovaMsg = `sova: ${host(cfg.rpc)}: ${errText(s.reason) === 'Failed to fetch' ? 'unreachable' : errText(s.reason)}`;
  }
  drawNums();
  drawRail();
  drawStatus();
  busy = false;
  nextAt = Date.now() + cfg.pollMs;
  timer = window.setTimeout(() => !document.hidden && tick(), cfg.pollMs);
}

setInterval(() => {
  if (lastBatchMs && info) $('last').textContent = `${ago(Date.now() - lastBatchMs)} · ${utc(lastBatchMs)}`;
  if (st.near === 'err' || st.sova === 'err') {
    const left = Math.max(0, Math.ceil((nextAt - Date.now()) / 1000));
    $('retry').textContent = busy ? 'retrying…' : `retry now (auto in ${left} s)`;
  }
}, 1000);

document.addEventListener('visibilitychange', () => {
  if (!document.hidden && Date.now() >= nextAt) tick();
});
$('retry').addEventListener('click', () => tick());
$('more').addEventListener('click', async () => {
  const btn = $('more') as HTMLButtonElement;
  btn.disabled = true;
  try {
    const from = Math.max(0, oldest - cfg.page);
    addRows(await batches(from, oldest - from), 'bottom', false);
  } catch (e) {
    st.near = 'err';
    st.nearMsg = `near: ${errText(e)}`;
    drawStatus();
  }
  btn.disabled = false;
});

// Archive account links and config line.
for (const a of document.querySelectorAll<HTMLAnchorElement>('a[data-acct]')) {
  a.href = `${cfg.nearExplorer}/address/${encodeURIComponent(cfg.contract)}`;
  a.textContent = cfg.contract;
}
$('cfg').textContent = `contract ${cfg.contract} · near ${cfg.near.map(host).join(', ')} · sova ${host(cfg.rpc)}`;

if (!ACCOUNT.test(cfg.contract)) {
  st.near = 'err';
  st.nearMsg = `near: "${cfg.contract}" is not a NEAR account name`;
  drawStatus();
  $('retry').hidden = true;
} else {
  tick();
}
