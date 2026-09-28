// /ashwings/mint: three steps, one button each, then the owl.
//   1 connect   an account, and the wallet on Sova testnet (added if the
//               wallet doesn't know it; "+ add Sova testnet" works before
//               connecting too). A wallet on another network shows "switch".
//   2 get SOVA  done when the wallet holds the price plus gas; else the
//               relayer's testnet drip tops it up (POST /drip), else links
//               to mining and the TAZ checkout.
//   3 mint      Ashwings.mint() for the price; the owl replaces the hero.
// A "no" in the wallet turns that step's button into "try again".
// Below: the gallery of minted owls (newest first).
import {
  type Owl, SEL, config, collection, owls, sales, wallet, connect, send, sentFor, why, sova, zecOf, card, setStatus, short,
} from './owls';
import { rpc, waitingForBlock } from '../checkout/chain';
import {
  type Eip1193, WalletError, chainSpec, ensureChain, knownAccount, walletAppLinks, walletChain, watchWallet,
} from '../checkout/wallet';

const PAGE = 12;
// Headroom over the price for the mint's gas. The testnet base fee is a few
// wei, so this is generous; the drip gives 0.1 SOVA over the price.
const GAS = 10n ** 16n;
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const root = $('mint');
const cfg = config(root);
const status = $('status');
const samples = JSON.parse(root.dataset.samples || '[]') as string[];

type Step = 'connect' | 'add' | 'drip' | 'mint';
const S = {
  eth: undefined as Eip1193 | undefined,
  checked: false, // looked for a wallet yet
  wallet: '',
  chain: '',
  want: '',
  bal: null as bigint | null,
  supply: 0n,
  max: 10000n,
  price: 0n,
  left: 1n,
  busy: '' as '' | Step,
  retry: '' as '' | Step,
  drip: 'unknown' as 'unknown' | 'on' | 'off',
  minted: null as Owl | null,
  mintTx: '',
  from: 0n,
};

/** SOVA for a balance line: at most 4 decimals ("625.1", "0.0999"). */
const sova4 = (wei: bigint) => sova(wei - (wei % 10n ** 14n)) || '0';

const onSova = () => Boolean(S.wallet) && S.want !== '' && S.chain === S.want;
const enough = () => S.bal !== null && S.price > 0n && S.bal >= S.price + GAS;

// ---- render -------------------------------------------------------------------

function el(tag: string, text: string, attrs: Record<string, string> = {}) {
  const e = document.createElement(tag);
  e.textContent = text;
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  return e;
}
function info(id: string, ...parts: (Node | string)[]) {
  $(id).replaceChildren(...parts);
}
/** A step's button: shown or not, primary (gold) or not, its label or "try again". */
function button(id: string, primary: boolean, show = true, label = '') {
  const b = $<HTMLButtonElement>(id);
  b.dataset.label ??= b.innerHTML;
  b.hidden = !show;
  b.classList.toggle('go', primary);
  b.disabled = S.busy !== '' || S.left <= 0n;
  const want = label || b.dataset.label;
  if (b.innerHTML !== want) b.innerHTML = want;
}

function render() {
  const connected = Boolean(S.wallet);
  const on = onSova();
  const has = enough();
  const s1 = on ? 'done' : 'cur';
  const s2 = !on ? 'todo' : has || S.minted ? 'done' : 'cur';
  const s3 = !on || !has ? (S.minted ? 'done' : 'todo') : 'cur';
  $('s1').dataset.s = s1;
  $('s2').dataset.s = s2;
  $('s3').dataset.s = s3;

  // 1 connect
  const retry = S.retry === 'connect' ? 'try again' : '';
  if (!S.eth) {
    button('b-connect', false, true);
    $<HTMLButtonElement>('b-connect').disabled = true;
    $('b-add').hidden = true;
    const kids: (Node | string)[] = [S.checked ? 'no wallet in this browser.' : 'looking for a wallet…'];
    if (S.checked && matchMedia('(pointer: coarse)').matches) {
      kids.push(' open this page in ');
      walletAppLinks().forEach((l, i) => kids.push(i ? ' or ' : '', el('a', l.label, { href: l.href })));
    }
    info('i1', ...kids);
  } else if (on) {
    button('b-connect', false, false);
    $('b-add').hidden = true;
    info('i1', el('span', short(S.wallet), { class: 'me' }), ' · Sova testnet');
  } else if (connected) {
    button('b-connect', true, true, retry || 'switch to Sova testnet');
    $('b-add').hidden = true;
    info('i1', el('span', `! wallet on another network (${S.chain ? BigInt(S.chain).toString() : '?'})`, { class: 'bad' }));
  } else {
    button('b-connect', true, true, retry);
    $('b-add').hidden = false;
    $<HTMLButtonElement>('b-add').disabled = S.busy !== '';
    info('i1', 'any browser wallet: MetaMask, Rabby, Coinbase Wallet…');
  }

  // 2 get SOVA
  const need = S.price + GAS - (S.bal ?? 0n);
  if (s2 === 'done') {
    button('b-drip', false, false);
    info('i2', `${sova4(S.bal ?? 0n)} SOVA`);
  } else if (s2 === 'cur' && S.drip === 'on') {
    button('b-drip', true, true, S.retry === 'drip' ? 'try again' : '');
    info('i2', 'free test SOVA · one drip per address');
  } else if (s2 === 'cur') {
    button('b-drip', false, false);
    info('i2', `you need ${sova(need > 0n ? need : S.price)} more SOVA: `, el('a', 'mine it', { href: '/mine' }), ' or pay with TAZ below');
  } else {
    button('b-drip', false, S.drip !== 'off');
    $<HTMLButtonElement>('b-drip').disabled = true;
    info('i2', '');
  }
  document.querySelectorAll('.need').forEach((b) => (b.textContent = S.price ? sova(S.price) : '…'));

  // 3 mint
  button('go-sova', s3 === 'cur', s3 !== 'done', S.retry === 'mint' ? 'try again' : '');
  $<HTMLButtonElement>('go-sova').disabled = s3 !== 'cur' || S.busy !== '' || S.left <= 0n;
  const p = document.getElementById('p-sova');
  if (p) p.textContent = S.price ? sova(S.price) : '…';
  if (S.minted) {
    const kids: (Node | string)[] = [el('span', `${S.minted.name} is yours`, { class: 'me' })];
    if (cfg.explorer && S.mintTx) kids.push(' · ', el('a', 'tx', { href: `${cfg.explorer}/tx/${S.mintTx}` }));
    info('i3', ...kids);
  } else info('i3', '');
}

// ---- reads ----------------------------------------------------------------------

async function refresh() {
  const c = await collection(cfg);
  S.supply = c.supply;
  S.max = c.max;
  S.price = c.price;
  S.left = c.max - c.supply - c.held;
  const pct = Number((c.supply * 10000n) / c.max) / 100;
  $('n').textContent = c.supply.toLocaleString('en-US');
  $('max').textContent = c.max.toLocaleString('en-US');
  $('fill').style.width = `${Math.max(pct, c.supply > 0n ? 0.6 : 0)}%`;
  $('p-zec').textContent = zecOf(c.zecPrice);
  const z = $<HTMLAnchorElement>('go-zec');
  const q = new URLSearchParams({ rpc: cfg.rpc, co: c.checkout, listing: '1' });
  if (cfg.relayer) q.set('relayer', cfg.relayer);
  z.href = `/ashwings/buy?${q}`;
  z.toggleAttribute('aria-disabled', S.left <= 0n);
  if (S.left <= 0n) setStatus(status, 'ok', 'sold out · see the market');
  if (S.wallet) S.bal = BigInt(await rpc<string>(cfg.rpc, 'eth_getBalance', [S.wallet, 'latest']));
  if (S.eth) S.chain = await walletChain(S.eth);
  render();
  return c;
}

async function balance() {
  if (!S.wallet) return (S.bal = null);
  S.bal = BigInt(await rpc<string>(cfg.rpc, 'eth_getBalance', [S.wallet, 'latest']));
  return S.bal;
}

/** Is the relayer's drip up? (tools/checkout-relayer GET /status: drip.accepting) */
async function dripUp() {
  if (!cfg.relayer) return (S.drip = 'off');
  try {
    const j = await (await fetch(`${cfg.relayer}/status`)).json();
    S.drip = j?.drip?.accepting ? 'on' : 'off';
  } catch {
    S.drip = 'off';
  }
  render();
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

// ---- the hero owl: the samples in turn, then yours ---------------------------------

let heroTimer = 0;
function cycleHero() {
  if (!samples.length || matchMedia('(prefers-reduced-motion: reduce)').matches) return;
  let i = 4;
  heroTimer = window.setInterval(() => {
    i = (i + 1) % samples.length;
    $<HTMLImageElement>('hero-img').src = samples[i];
  }, 900);
}
function reveal(o: Owl) {
  window.clearInterval(heroTimer);
  const img = $<HTMLImageElement>('hero-img');
  img.src = o.image;
  img.alt = o.name;
  img.title = o.traits;
  $('hero').classList.add('mine');
  $('hero-cap').textContent = `#${o.id} · yours`;
  $('hero-cap').hidden = false;
}

// ---- actions --------------------------------------------------------------------------

async function run(step: Step, fn: () => Promise<void>) {
  if (S.busy) return;
  S.busy = step;
  S.retry = '';
  render();
  try {
    await fn();
  } catch (e) {
    const no = e instanceof WalletError ? e.kind === 'rejected' : (e as any)?.code === 4001;
    // The failed step's button turns into "try again" (the add button has
    // no label to spare: the status line says it).
    S.retry = step === 'add' ? '' : step;
    setStatus(status, 'err', no ? 'rejected in wallet · try again when ready' : why(e));
  } finally {
    S.busy = '';
    if (S.eth) {
      S.wallet ||= await knownAccount(S.eth);
      S.chain = await walletChain(S.eth);
    }
    await balance().catch(() => {});
    render();
  }
}

const doConnect = () =>
  run('connect', async () => {
    if (!S.wallet) {
      setStatus(status, 'wait', 'connect · approve in your wallet');
      S.wallet = await connect(cfg);
    } else {
      setStatus(status, 'wait', 'switch · approve in your wallet');
      await ensureChain(S.eth!, await chainSpec(cfg));
    }
    S.chain = await walletChain(S.eth!);
    await balance();
    setStatus(status, 'ok', enough() ? 'connected · ready to mint' : 'connected · now get SOVA');
  });

const doAdd = () =>
  run('add', async () => {
    setStatus(status, 'wait', 'add Sova testnet · approve in your wallet');
    await ensureChain(S.eth!, await chainSpec(cfg));
    S.chain = await walletChain(S.eth!);
    setStatus(status, 'ok', 'Sova testnet is in your wallet · now connect');
  });

const doDrip = () =>
  run('drip', async () => {
    setStatus(status, 'wait', 'asking the drip');
    let res: Response;
    try {
      res = await fetch(`${cfg.relayer}/drip`, {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ address: S.wallet }),
      });
    } catch {
      throw new Error('the drip is busy: try again in a minute');
    }
    const j = await res.json().catch(() => ({}));
    if (res.status === 409 && /enough SOVA/.test(j.error || '')) {
      await balance();
      return setStatus(status, 'ok', 'you have enough SOVA · mint');
    }
    if (!res.ok) throw new Error(j.error || `drip: ${res.status}`);
    waitingForBlock(status, status.querySelector<HTMLElement>('.st')!, `${sova(BigInt(j.amountWei || 0))} SOVA sent · waiting for a block`);
    for (let i = 0; i < 400 && !enough(); i++) {
      await new Promise((ok) => setTimeout(ok, 1500));
      await balance().catch(() => {});
    }
    if (!enough()) throw new Error('the SOVA has not arrived after 10 min: reload later');
    setStatus(status, 'ok', 'SOVA in · now mint');
  });

const doMint = () =>
  run('mint', async () => {
    setStatus(status, 'wait', `minting · confirm ${sova(S.price)} SOVA in your wallet`);
    const rc = await send(cfg, S.wallet, cfg.ashw, '0x' + SEL.mint, S.price, sentFor(status, 'minting'));
    const log = rc.logs.find((l: any) => l.address.toLowerCase() === cfg.ashw.toLowerCase());
    const id = BigInt(log.topics[3]);
    const [o] = await owls(cfg, [id]);
    S.minted = o;
    S.mintTx = rc.transactionHash;
    reveal(o);
    setStatus(status, 'ok', `minted · ${o.name} is yours`);
    await refresh();
    await gallery(S.supply);
  });

// ---- boot -------------------------------------------------------------------------------

$('b-connect').addEventListener('click', doConnect);
$('b-add').addEventListener('click', doAdd);
$('b-drip').addEventListener('click', doDrip);
$('go-sova').addEventListener('click', doMint);
$('more').addEventListener('click', () => gallery(S.from, true).catch((e) => setStatus(status, 'err', why(e))));
$('cfg').textContent = `rpc ${cfg.rpc} · ashwings ${cfg.ashw} · market ${cfg.market} · relayer ${cfg.relayer || 'none'}`;
$('to-market').setAttribute('href', `/ashwings/market${location.search}`);
cycleHero();
render();

async function boot() {
  S.eth = await wallet();
  S.checked = true;
  try {
    S.want = (await chainSpec(cfg)).chainId;
  } catch (e) {
    return setStatus(status, 'err', `${why(e)} · rpc ${cfg.rpc}`);
  }
  if (S.eth) {
    // Already connected on an earlier visit: no prompt, just show where it is.
    S.wallet = await knownAccount(S.eth);
    S.chain = await walletChain(S.eth);
    watchWallet(S.eth, {
      chain: (id) => {
        const was = onSova();
        S.chain = id;
        if (!S.busy && S.wallet && was !== onSova()) {
          if (onSova()) setStatus(status, 'ok', 'back on Sova testnet');
          else setStatus(status, 'err', 'wallet on another network · switch to Sova testnet');
        }
        render();
      },
      account: (a) => {
        S.wallet = a;
        S.bal = null;
        balance().catch(() => {}).finally(render);
      },
    });
  }
  dripUp();
  try {
    const c = await refresh();
    if (c.max - c.supply - c.held > 0n) {
      setStatus(status, 'wait', !S.eth ? 'no wallet here · pay with TAZ below' : onSova() ? (enough() ? 'ready · mint' : 'ready · get SOVA') : 'ready · connect');
    }
    await gallery(c.supply);
  } catch (e) {
    setStatus(status, 'err', `${why(e)} · rpc ${cfg.rpc}`);
  }
}
boot();
setInterval(() => {
  if (!S.busy) refresh().catch(() => {});
}, 6000);
