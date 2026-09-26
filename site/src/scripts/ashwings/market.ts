// /ashwings/market: owls for sale (buy) and, with a wallet, your owls
// (list: approve this owl for the market, then list; cancel).
// Listings are found from the market's Listed logs and checked live.
import {
  type Owl, type Sale, SEL, config, owls, sales, listedIds, ownedBy, eth, connect, send, sentFor, why, sova, parseSova,
  card, setStatus, one,
} from './owls';
import { u256, addrWord, calldata, words, asAddr } from '../checkout/chain';

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const cfg = config($('market'));
const status = $('status');
const S = { wallet: '', busy: false, saleSig: '' };

function el<K extends keyof HTMLElementTagNameMap>(tag: K, props: Partial<HTMLElementTagNameMap[K]> = {}, ...kids: (Node | string)[]) {
  const e = Object.assign(document.createElement(tag), props);
  e.append(...kids);
  return e;
}

async function act(label: string, fn: () => Promise<unknown>) {
  if (S.busy) return;
  S.busy = true;
  try {
    if (!S.wallet) await doConnect();
    setStatus(status, 'wait', `${label} · confirm in your wallet`);
    await fn();
    setStatus(status, 'ok', `${label} · done`);
    await load();
  } catch (e) {
    setStatus(status, 'err', why(e));
  } finally {
    S.busy = false;
  }
}

const buy = (s: Sale) =>
  act(`buying #${s.id} for ${sova(s.price)} SOVA`, () =>
    send(cfg, S.wallet, cfg.market, calldata(SEL.buy, u256(s.id)), s.price, sentFor(status, `buying #${s.id}`)));

const cancel = (id: bigint) =>
  act(`canceling #${id}`, () => send(cfg, S.wallet, cfg.market, calldata(SEL.cancel, u256(id)), 0n, sentFor(status, `canceling #${id}`)));

function list(id: bigint, input: HTMLInputElement) {
  const price = parseSova(input.value);
  if (price === null) return setStatus(status, 'err', 'price in SOVA, e.g. 25 or 12.5');
  return act(`listing #${id} at ${sova(price)} SOVA`, async () => {
    const approved = asAddr(words(await one(cfg.rpc, cfg.ashw, calldata(SEL.getApproved, u256(id))))[0]);
    if (approved.toLowerCase() !== cfg.market.toLowerCase()) {
      setStatus(status, 'wait', `1/2 approve the market for #${id} · confirm in your wallet`);
      await send(cfg, S.wallet, cfg.ashw, calldata(SEL.approve, addrWord(cfg.market), u256(id)), 0n, sentFor(status, `1/2 approving #${id}`));
      setStatus(status, 'wait', `2/2 list #${id} · confirm in your wallet`);
    }
    await send(cfg, S.wallet, cfg.market, calldata(SEL.list, u256(id), u256(price)), 0n, sentFor(status, `listing #${id}`));
  });
}

function saleRow(o: Owl, s: Sale): HTMLElement {
  const mine = S.wallet && s.seller.toLowerCase() === S.wallet.toLowerCase();
  const btn = el('button', { type: 'button', className: mine ? '' : 'go', textContent: mine ? 'cancel' : 'buy' });
  btn.addEventListener('click', () => (mine ? cancel(o.id) : buy(s)));
  return el('div', { className: 'row' }, el('span', { className: 'price', textContent: `${sova(s.price)} SOVA` }), btn);
}

function ownRow(o: Owl, s?: Sale): HTMLElement {
  if (s) return saleRow(o, s);
  const input = el('input', { type: 'text', inputMode: 'decimal', placeholder: 'SOVA' });
  input.setAttribute('aria-label', `price for #${o.id} in SOVA`);
  const btn = el('button', { type: 'button', textContent: 'list' });
  btn.addEventListener('click', () => list(o.id, input));
  input.addEventListener('keydown', (e) => e.key === 'Enter' && list(o.id, input));
  return el('div', { className: 'row' }, input, btn);
}

/** The for-sale grid; re-rendered only when a listing changed. */
async function loadSales(force = false) {
  const ids = await listedIds(cfg);
  const live = await sales(cfg, ids);
  const sig = S.wallet + [...live.values()].map((s) => `${s.id}:${s.seller}:${s.price}`).join(',');
  if (!force && sig === S.saleSig) return;
  S.saleSig = sig;
  const forSale = [...live.keys()];
  const cards = await owls(cfg, forSale);
  const grid = $('sale');
  grid.replaceChildren(...cards.map((o) => {
    const li = card(o, S.wallet, saleRow(o, live.get(o.id)!));
    li.id = `o${o.id}`;
    return li;
  }));
  $('n-sale').textContent = String(cards.length);
  const floor = [...live.values()].reduce((m, s) => (m === 0n || s.price < m ? s.price : m), 0n);
  $('floor').textContent = floor ? `floor ${sova(floor)} SOVA` : '';
}

/** Everything: for sale, and (with a wallet) your owls. */
async function load() {
  await loadSales(true);
  if (!S.wallet) return;
  const mineIds = await ownedBy(cfg, S.wallet);
  const [mine, mySales] = await Promise.all([owls(cfg, mineIds), sales(cfg, mineIds)]);
  $('mine').replaceChildren(...mine.map((o) => card(o, S.wallet, ownRow(o, mySales.get(o.id)))));
  $('mine-h').hidden = false;
  $('mine').hidden = false;
}

async function doConnect() {
  S.wallet = await connect(cfg);
  $('connect').textContent = 'wallet ' + S.wallet.slice(0, 6) + '…' + S.wallet.slice(-4);
}

$('cfg').textContent = `rpc ${cfg.rpc} · ashwings ${cfg.ashw} · market ${cfg.market}`;
$('to-mint').setAttribute('href', `/ashwings/mint${location.search}`);
if (eth) {
  $('connect').hidden = false;
  $('connect').addEventListener('click', () =>
    doConnect()
      .then(load)
      .then(() => setStatus(status, 'ok', 'wallet connected'))
      .catch((e) => setStatus(status, 'err', why(e))),
  );
}
load()
  .then(() => {
    setStatus(status, 'wait', eth ? 'connect a wallet to buy or list' : 'read-only · no wallet in this browser');
    if (location.hash) document.getElementById(location.hash.slice(1))?.scrollIntoView();
  })
  .catch((e) => setStatus(status, 'err', `${why(e)} · rpc ${cfg.rpc}`));
// Poll the for-sale grid only: your grid holds price inputs being typed.
setInterval(() => {
  if (!S.busy) loadSales().catch(() => {});
}, 8000);
