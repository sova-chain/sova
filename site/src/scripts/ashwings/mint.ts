// /ashwings/mint: supply, mint for SOVA with an injected wallet, hand off
// to /ashwings/buy for ZEC, and the gallery of minted owls (newest first).
import {
  type Owl, SEL, config, collection, owls, sales, eth, connect, send, why, sova, zecOf, card, setStatus,
} from './owls';

const PAGE = 12;
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const root = $('mint');
const cfg = config(root);
const status = $('status');
const S = { wallet: '', supply: 0n, max: 10000n, price: 0n, busy: false, from: 0n };

function zecHref(checkout: string) {
  const q = new URLSearchParams({ rpc: cfg.rpc, co: checkout, listing: '1' });
  if (cfg.relayer) q.set('relayer', cfg.relayer);
  return `/ashwings/buy?${q}`;
}

async function refresh() {
  const c = await collection(cfg);
  S.supply = c.supply;
  S.max = c.max;
  S.price = c.price;
  const pct = Number((c.supply * 10000n) / c.max) / 100;
  $('n').textContent = c.supply.toLocaleString('en-US');
  $('max').textContent = c.max.toLocaleString('en-US');
  $('fill').style.width = `${Math.max(pct, c.supply > 0n ? 0.6 : 0)}%`;
  $('held').textContent = c.held > 0n ? `${c.held} held for open zec orders` : '';
  const left = c.max - c.supply - c.held;
  $<HTMLButtonElement>('go-sova').disabled = left <= 0n;
  $('p-sova').textContent = sova(c.price);
  $('p-zec').textContent = zecOf(c.zecPrice);
  const z = $<HTMLAnchorElement>('go-zec');
  z.href = zecHref(c.checkout);
  z.toggleAttribute('aria-disabled', left <= 0n);
  if (left <= 0n) setStatus(status, 'ok', 'sold out · see the market');
  return c;
}

/** Newest-first gallery page starting at id `from` (inclusive). */
async function gallery(from: bigint, append = false) {
  const ids: bigint[] = [];
  for (let id = from; id >= 1n && ids.length < PAGE; id--) ids.push(id);
  const grid = $('grid');
  if (!append) grid.replaceChildren();
  const [list, forSale] = await Promise.all([owls(cfg, ids), sales(cfg, ids).catch(() => new Map())]);
  for (const o of list) {
    const s = forSale.get(o.id);
    let tag: HTMLElement | undefined;
    if (s) {
      tag = document.createElement('a');
      tag.className = 'tag';
      tag.textContent = `${sova(s.price)} SOVA`;
      (tag as HTMLAnchorElement).href = `/ashwings/market${location.search}#o${o.id}`;
    }
    grid.append(card(o, S.wallet, tag));
  }
  S.from = from - BigInt(ids.length);
  $('more').hidden = S.from < 1n;
  $('empty').hidden = S.supply > 0n;
}

function reveal(o: Owl) {
  const img = $<HTMLImageElement>('new-img');
  img.src = o.image;
  img.alt = o.name;
  img.title = o.traits;
  $('new-name').textContent = o.name;
  $('new').hidden = false;
}

async function mintSova() {
  if (S.busy) return;
  S.busy = true;
  const btn = $<HTMLButtonElement>('go-sova');
  btn.disabled = true;
  try {
    if (!S.wallet) S.wallet = await connect(cfg);
    setStatus(status, 'wait', `minting · confirm ${sova(S.price)} SOVA in your wallet`);
    const rc = await send(cfg, S.wallet, cfg.ashw, '0x' + SEL.mint, S.price);
    const log = rc.logs.find((l: any) => l.address.toLowerCase() === cfg.ashw.toLowerCase());
    const id = BigInt(log.topics[3]);
    const [o] = await owls(cfg, [id]);
    reveal(o);
    setStatus(status, 'ok', `minted · ${o.name} is yours`);
    await refresh();
    await gallery(S.supply);
  } catch (e) {
    setStatus(status, 'err', why(e));
  } finally {
    S.busy = false;
    btn.disabled = S.max - S.supply <= 0n;
  }
}

async function boot() {
  try {
    const c = await refresh();
    if (c.max - c.supply - c.held > 0n) setStatus(status, 'wait', eth ? 'ready · wallet or zcash' : 'no wallet here · pay with zec');
    await gallery(c.supply);
  } catch (e) {
    setStatus(status, 'err', `${why(e)} · rpc ${cfg.rpc}`);
  }
}

$('go-sova').addEventListener('click', mintSova);
$('more').addEventListener('click', () => gallery(S.from, true).catch((e) => setStatus(status, 'err', why(e))));
$('cfg').textContent = `rpc ${cfg.rpc} · ashwings ${cfg.ashw} · market ${cfg.market}`;
$('to-market').setAttribute('href', `/ashwings/market${location.search}`);
if (!eth) $('go-sova').title = 'needs an injected wallet (window.ethereum)';
boot();
setInterval(() => {
  if (!S.busy) refresh().catch(() => {});
}, 6000);
