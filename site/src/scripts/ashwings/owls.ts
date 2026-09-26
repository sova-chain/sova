// Shared by /ashwings/mint and /ashwings/market: config, batched JSON-RPC
// reads, an injected-wallet sender, and the owl card. No library: the
// selectors are precomputed (`cast sig` / `cast keccak`) and the art is the
// contract's own tokenURI.
//
// Config: the page's data-config, overridden by ?rpc=&ashw=&market=&relayer=
// (defaults: the public testnet, contracts from
// infra/testnet/deployments/sova-testnet.json).
import {
  RpcError, rpc, useChain, u256, addrWord, calldata, words, num, asAddr, dynBytes, utf8, waitingForBlock, stopWaiting,
} from '../checkout/chain';

export type Cfg = { rpc: string; chainId?: number; ashw: string; market: string; relayer: string };
export type Owl = { id: bigint; name: string; image: string; traits: string; owner: string };
export type Sale = { id: bigint; seller: string; price: bigint };

export const SEL = {
  totalSupply: '18160ddd',
  maxSupply: '32cb6b0c',
  priceWei: 'b7ec2086',
  zecHeld: 'ec9a5147',
  zecCheckout: 'c4f85e7b',
  mint: '1249c58b',
  tokenURI: 'c87b56dd',
  ownerOf: '6352211e',
  getApproved: '081812fc',
  approve: '095ea7b3',
  listings: 'de74e57b', // both AshwingsMarket.listings and ZecCheckout.listings
  list: '50fd7367',
  cancel: '40e58ee5',
  buy: 'd96a094a',
  isLive: '27507458',
};

export const TOPIC = {
  transfer: '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef',
  listed: '0x50955776c5778c3b7d968d86d8c51fb6b29a7a74c20866b533268e209fc08343',
};

const ERRORS: Record<string, string> = {
  '30cd7471': 'not your owl',
  c19f17a9: 'approve the market for this owl first',
  fd1ee349: 'price must be above zero',
  '665c1c57': 'not listed',
  '5ec82351': 'only the seller can cancel a live listing',
  '195a3ca1': 'the price changed: reload',
  '4b19d294': 'listing is stale: the owl moved or its approval changed',
  f499da20: 'payment failed',
  ab143c06: 'reentrancy blocked',
};

/** Readable revert reason: market custom errors, "ASHW: ..." strings, wallet messages. */
export function why(e: unknown): string {
  const data = e instanceof RpcError ? e.data : (e as any)?.data?.data ?? (e as any)?.data;
  const msg = e instanceof Error ? e.message : String((e as any)?.message ?? e);
  const hex = (typeof data === 'string' ? data : msg.match(/0x[0-9a-f]{8}/i)?.[0] || '').replace(/^0x/, '').slice(0, 8).toLowerCase();
  if (ERRORS[hex]) return ERRORS[hex];
  const ashw = msg.match(/ASHW: [a-z ]+/);
  if (ashw) return ashw[0].replace('ASHW: ', '');
  if ((e as any)?.code === 4001) return 'rejected in wallet';
  return msg.replace(/^execution reverted:?\s*/i, '').slice(0, 160) || 'reverted';
}

export function config(root: HTMLElement): Cfg {
  const q = new URLSearchParams(location.search);
  const base = JSON.parse(root.dataset.config || '{}') as Cfg;
  return {
    rpc: q.get('rpc') || base.rpc,
    chainId: base.chainId,
    ashw: q.get('ashw') || base.ashw,
    market: q.get('market') || base.market,
    relayer: q.get('relayer') || base.relayer || '',
  };
}

// ---- reads ------------------------------------------------------------------

let bseq = 0;
/** eth_call many in one JSON-RPC batch; failed calls come back as null. */
export async function calls(url: string, reqs: { to: string; data: string }[]): Promise<(string | null)[]> {
  if (!reqs.length) return [];
  const body = reqs.map((r) => ({ jsonrpc: '2.0', id: ++bseq, method: 'eth_call', params: [r, 'latest'] }));
  const res = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  const out = (await res.json()) as any[];
  const byId = new Map(out.map((o) => [o.id, o]));
  return body.map((b) => byId.get(b.id)?.result ?? null);
}

export const one = (url: string, to: string, data: string) => rpc<string>(url, 'eth_call', [{ to, data }, 'latest']);
export const word = async (url: string, to: string, sel: string) => num(words(await one(url, to, '0x' + sel))[0]);

export type Collection = { supply: bigint; max: bigint; price: bigint; held: bigint; checkout: string; zecPrice: bigint };
export async function collection(c: Cfg): Promise<Collection> {
  const [s, m, p, h, z] = await calls(c.rpc, [SEL.totalSupply, SEL.maxSupply, SEL.priceWei, SEL.zecHeld, SEL.zecCheckout].map(
    (sel) => ({ to: c.ashw, data: '0x' + sel }),
  ));
  if (s === null || s === '0x') throw new Error(`no Ashwings contract at ${c.ashw}`);
  const checkout = asAddr(words(z!)[0]);
  // ZecCheckout.listings(1): (seller, priceZat, minConf, p2sh, active, payeeHash, window)
  const l = words(await one(c.rpc, checkout, calldata(SEL.listings, u256(1))));
  return { supply: num(words(s)[0]), max: num(words(m!)[0]), price: num(words(p!)[0]), held: num(words(h!)[0]), checkout, zecPrice: num(l[1]) };
}

/** Owl cards for `ids`: metadata from tokenURI, plus the owner. */
export async function owls(c: Cfg, ids: bigint[]): Promise<Owl[]> {
  const reqs = ids.flatMap((id) => [
    { to: c.ashw, data: calldata(SEL.tokenURI, u256(id)) },
    { to: c.ashw, data: calldata(SEL.ownerOf, u256(id)) },
  ]);
  const res = await calls(c.rpc, reqs);
  const out: Owl[] = [];
  ids.forEach((id, i) => {
    const uri = res[2 * i];
    if (!uri) return;
    const meta = JSON.parse(atob(utf8(dynBytes(uri, 0)).split(',')[1]));
    out.push({
      id,
      name: meta.name,
      image: meta.image,
      traits: (meta.attributes || []).map((a: any) => `${a.trait_type}: ${a.value}`).join('\n'),
      owner: asAddr(words(res[2 * i + 1]!)[0]),
    });
  });
  return out;
}

/** Live market listings among `ids` (stale and empty ones dropped). */
export async function sales(c: Cfg, ids: bigint[]): Promise<Map<bigint, Sale>> {
  const res = await calls(c.rpc, ids.flatMap((id) => [
    { to: c.market, data: calldata(SEL.listings, u256(id)) },
    { to: c.market, data: calldata(SEL.isLive, u256(id)) },
  ]));
  const m = new Map<bigint, Sale>();
  ids.forEach((id, i) => {
    const l = res[2 * i];
    const live = res[2 * i + 1];
    if (!l || !live || num(words(live)[0]) !== 1n) return;
    const w = words(l);
    m.set(id, { id, seller: asAddr(w[0]), price: num(w[1]) });
  });
  return m;
}

/** Ids ever listed on the market (from Listed logs), newest first. */
export async function listedIds(c: Cfg): Promise<bigint[]> {
  const logs = await rpc<any[]>(c.rpc, 'eth_getLogs', [{ address: c.market, topics: [TOPIC.listed], fromBlock: '0x0', toBlock: 'latest' }]);
  return [...new Set(logs.map((l) => num(l.topics[1].slice(2))))].reverse();
}

/** Ids `who` owns now: everything ever transferred to them, checked against ownerOf. */
export async function ownedBy(c: Cfg, who: string): Promise<bigint[]> {
  const logs = await rpc<any[]>(c.rpc, 'eth_getLogs', [{
    address: c.ashw, topics: [TOPIC.transfer, null, '0x' + addrWord(who)], fromBlock: '0x0', toBlock: 'latest',
  }]);
  const ids = [...new Set(logs.map((l) => num(l.topics[3].slice(2))))];
  const res = await calls(c.rpc, ids.map((id) => ({ to: c.ashw, data: calldata(SEL.ownerOf, u256(id)) })));
  return ids.filter((_, i) => res[i] && asAddr(words(res[i]!)[0]).toLowerCase() === who.toLowerCase()).reverse();
}

// ---- wallet -------------------------------------------------------------------

type Eth = { request(a: { method: string; params?: unknown[] }): Promise<any> };
export const eth = (window as any).ethereum as Eth | undefined;

export async function connect(c: Cfg): Promise<string> {
  if (!eth) throw new Error('no wallet in this browser');
  const [a] = await eth.request({ method: 'eth_requestAccounts' });
  await useChain(eth, c.rpc, c.chainId);
  return a;
}

/**
 * Dry-run, send from the wallet, wait for the receipt. `onSent` runs once
 * the wallet has sent it (show the waiting state there). A Sova block
 * follows each Zcash block, so this waits up to 10 min.
 */
export async function send(c: Cfg, from: string, to: string, data: string, value = 0n, onSent?: () => void) {
  const tx = { from, to, data, value: '0x' + value.toString(16) };
  await rpc(c.rpc, 'eth_call', [tx, 'latest']); // a revert surfaces here, with its reason
  const hash: string = await eth!.request({ method: 'eth_sendTransaction', params: [tx] });
  onSent?.();
  for (let i = 0; i < 400; i++) {
    const r = await rpc<any>(c.rpc, 'eth_getTransactionReceipt', [hash]);
    if (r) {
      if (r.status !== '0x1') throw new Error('transaction reverted');
      return r;
    }
    await new Promise((ok) => setTimeout(ok, 1500));
  }
  throw new Error(`not in a block after 10 min · tx ${hash.slice(0, 10)}… may still land: reload later`);
}

/** `onSent` for `send`: "<label> · sent, waiting for a block", with the clock and note. */
export const sentFor = (el: HTMLElement, label: string) => () =>
  waitingForBlock(el, el.querySelector<HTMLElement>('.st')!, `${label} · sent, waiting for a block`);

// ---- formatting -------------------------------------------------------------

/** Wei as SOVA, trailing zeros trimmed ("10", "24.75"). */
export function sova(wei: bigint): string {
  const f = (wei % 10n ** 18n).toString().padStart(18, '0').replace(/0+$/, '');
  return `${(wei / 10n ** 18n).toLocaleString('en-US')}${f ? '.' + f : ''}`;
}
/** "24.75" -> wei; null if not a positive decimal with <= 18 places. */
export function parseSova(s: string): bigint | null {
  const m = s.trim().match(/^(\d{1,12})(?:\.(\d{1,18}))?$/);
  if (!m) return null;
  const v = BigInt(m[1]) * 10n ** 18n + BigInt((m[2] || '').padEnd(18, '0') || '0');
  return v > 0n ? v : null;
}
export const zecOf = (zat: bigint) => `${zat / 100000000n}.${(zat % 100000000n).toString().padStart(8, '0')}`.replace(/\.?0+$/, '');
export const short = (a: string) => a.slice(0, 6) + '…' + a.slice(-4);

/** An owl card: image, #id, owner, and an optional action row. */
export function card(o: Owl, me: string, extra?: HTMLElement): HTMLElement {
  const li = document.createElement('li');
  li.className = 'owl';
  li.dataset.id = String(o.id);
  const img = document.createElement('img');
  img.src = o.image;
  img.alt = o.name;
  img.title = o.traits;
  img.width = 144;
  img.height = 144;
  img.loading = 'lazy';
  const cap = document.createElement('p');
  cap.className = 'cap';
  const mine = me && o.owner.toLowerCase() === me.toLowerCase();
  cap.innerHTML = `<b>#${o.id}</b> <span class="${mine ? 'me' : 'dim'}">${mine ? 'yours' : short(o.owner)}</span>`;
  li.append(img, cap);
  if (extra) li.append(extra);
  return li;
}

export function setStatus(el: HTMLElement, kind: 'wait' | 'ok' | 'err', text: string) {
  stopWaiting(el);
  el.dataset.k = kind;
  el.querySelector('.st')!.textContent = text;
}
