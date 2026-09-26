// Tiny JSON-RPC + ABI helpers for the checkout page: just the calls it
// makes, with selectors precomputed (`cast sig` / `cast keccak`), so the
// page needs no keccak and no library.

export const ZCASH = '0x0000000000000000000000000000000000005a00';
export const ZERO_ADDR = '0x' + '0'.repeat(40);

export const SEL = {
  reserve: '03339bcb', // reserve(uint256,address)
  claim: '8feddd7d', // claim(uint256,bytes32,uint32)
  reservations: '067cf832', // reservations(uint256)
  ashwings: '808bf757', // ashwings()
  zecCheckout: 'c4f85e7b', // Ashwings.zecCheckout()
  tokenURI: 'c87b56dd', // tokenURI(uint256)
  anchor: 'd3fb73b4', // anchor()
  txInfo: '0ac6923d', // txInfo(bytes32)
  txOutput: '2d45828e', // txOutput(bytes32,uint32)
};

export const TOPIC = {
  reserved: '0x231a5dd349a4e7525cc746a6d863526351b29114ff4c0203d4ec3a13589be824',
  claimed: '0xf33978e045cf9b34915c4f70b83a797b994a5cbb21c5c174ab983ed3639aee6c',
};

/** Custom-error selectors (ZecCheckout + ZcashLib) -> plain words. */
const ERRORS: Record<string, string> = {
  '89603337': 'no such order',
  '41a26a63': 'order already filled',
  '1103bbdd': 'that payment already filled another order',
  '9b998171': 'wrong amount: pay the exact quote',
  e9927223: 'payment was mined before the order',
  '780b81cd': 'payment was mined after the deadline',
  a45a5e44: 'Sova does not see that tx yet',
  edc0cab3: 'Sova does not see that tx yet',
  '3724267f': 'tx is older than the Zcash segment Sova indexes',
  cb7d45d7: 'tx has no such output',
  '93ef3b03': 'not enough confirmations yet',
  '892da3e0': 'output pays a different address',
  '3fe9a254': 'output pays too little',
  d18cffc8: 'listing is paused',
  d27b4443: 'no recipient address',
  b731f409: 'listing is out of tags',
};

let seq = 0;
export class RpcError extends Error {
  constructor(message: string, readonly data?: string) {
    super(message);
  }
}

export async function rpc<T = any>(url: string, method: string, params: unknown[] = []): Promise<T> {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: ++seq, method, params }),
  });
  const j = await res.json();
  if (j.error) throw new RpcError(j.error.message || 'rpc error', typeof j.error.data === 'string' ? j.error.data : j.error.data?.data);
  return j.result as T;
}

type Eth = { request(a: { method: string; params?: unknown[] }): Promise<any> };

/**
 * Put the wallet on the chain `rpcUrl` serves: switch to it, or add it first
 * (EIP-3085) when the wallet doesn't know it yet (error 4902). The public
 * testnet (`testnetChainId`, 82330 from the deploy record) is added as "Sova
 * testnet" with the public RPC; any other chain (a ?rpc= override) is added
 * under its own RPC URL. The currency is SOVA either way.
 */
export async function useChain(eth: Eth, rpcUrl: string, testnetChainId?: number): Promise<void> {
  const chain = await rpc<string>(rpcUrl, 'eth_chainId');
  if ((await eth.request({ method: 'eth_chainId' })) === chain) return;
  try {
    await eth.request({ method: 'wallet_switchEthereumChain', params: [{ chainId: chain }] });
  } catch (e: any) {
    // Some wallets wrap the 4902 (MetaMask mobile: data.originalError.code).
    if ((e?.code ?? e?.data?.originalError?.code) !== 4902) throw e;
    const testnet = testnetChainId !== undefined && BigInt(chain) === BigInt(testnetChainId);
    await eth.request({
      method: 'wallet_addEthereumChain',
      params: [{
        chainId: chain,
        chainName: testnet ? 'Sova testnet' : `Sova (${new URL(rpcUrl).host})`,
        nativeCurrency: { name: 'SOVA', symbol: 'SOVA', decimals: 18 },
        rpcUrls: [rpcUrl],
      }],
    });
  }
}

// ---- waiting for a block ----------------------------------------------------

/** The one line shown while a sent transaction waits for its block. */
export const BLOCK_NOTE = 'Sova makes one block per Zcash block: about a minute, sometimes several.';

const ticking = new WeakMap<HTMLElement, number>();

/**
 * A sent transaction is waiting for its block: `st` (the status text) says
 * so once, a clock counts up the elapsed time, and the note says why it
 * can take minutes. The clock is aria-hidden so screen readers hear the
 * status once, not every second. The status element holds `.clock` and
 * `.note` spans (the page's markup); any later status update calls
 * `stopWaiting`.
 */
export function waitingForBlock(status: HTMLElement, st: HTMLElement, text: string): void {
  stopWaiting(status);
  status.dataset.k = 'wait';
  st.textContent = text;
  const clock = status.querySelector<HTMLElement>('.clock');
  const note = status.querySelector<HTMLElement>('.note');
  const t0 = Date.now();
  const draw = () => {
    const s = Math.floor((Date.now() - t0) / 1000);
    if (clock) clock.textContent = ` · ${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
  };
  draw();
  if (clock) clock.hidden = false;
  if (note) {
    note.textContent = BLOCK_NOTE;
    note.hidden = false;
  }
  ticking.set(status, window.setInterval(draw, 1000));
}

/** End the waiting state: stop the clock, hide it and the note. */
export function stopWaiting(status: HTMLElement): void {
  const t = ticking.get(status);
  if (t !== undefined) window.clearInterval(t);
  ticking.delete(status);
  status.querySelectorAll<HTMLElement>('.clock, .note').forEach((el) => (el.hidden = true));
}

/** Readable reason for a revert (custom error selector), else the raw message. */
export function reason(e: unknown): string {
  const data = e instanceof RpcError ? e.data : undefined;
  const msg = e instanceof Error ? e.message : String(e);
  const hex = (data || msg.match(/0x[0-9a-f]{8}/i)?.[0] || '').replace(/^0x/, '').slice(0, 8).toLowerCase();
  return ERRORS[hex] || msg.replace(/^execution reverted:?\s*/i, '') || 'reverted';
}

// ---- ABI ------------------------------------------------------------------

const strip = (h: string) => h.replace(/^0x/, '').toLowerCase();
export const u256 = (v: bigint | number) => BigInt(v).toString(16).padStart(64, '0');
export const addrWord = (a: string) => strip(a).padStart(64, '0');
export const b32 = (h: string) => strip(h).padStart(64, '0');
export const calldata = (sel: string, ...words: string[]) => '0x' + sel + words.join('');

/** Return data as 32-byte hex words. */
export function words(ret: string): string[] {
  const h = strip(ret);
  const out: string[] = [];
  for (let i = 0; i < h.length; i += 64) out.push(h.slice(i, i + 64));
  return out;
}
export const num = (w: string) => BigInt('0x' + (w || '0'));
export const asAddr = (w: string) => '0x' + w.slice(24);

/** Dynamic `bytes` / `string` whose head offset is at word `i`. */
export function dynBytes(ret: string, i: number): string {
  const h = strip(ret);
  const off = Number(num(h.slice(i * 64, i * 64 + 64))) * 2;
  const len = Number(num(h.slice(off, off + 64))) * 2;
  return h.slice(off + 64, off + 64 + len);
}
export const utf8 = (hex: string) =>
  new TextDecoder().decode(Uint8Array.from(hex.match(/../g) || [], (b) => parseInt(b, 16)));

// ---- Zcash ----------------------------------------------------------------

export const payeeScript = (hash: string, p2sh: boolean) => (p2sh ? `a914${hash}87` : `76a914${hash}88ac`);

const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const PREFIX = { test: { p2pkh: '1d25', p2sh: '1cba' }, main: { p2pkh: '1cb8', p2sh: '1cbd' } };

async function sha256(hex: string): Promise<string> {
  const bytes = Uint8Array.from(hex.match(/../g) || [], (b) => parseInt(b, 16));
  const d = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
  return Array.from(d, (b) => b.toString(16).padStart(2, '0')).join('');
}

/** Base58Check t-address for a 20-byte hash (t1/t3 mainnet, tm/t2 testnet). */
export async function tAddr(hash: string, p2sh: boolean, net: 'test' | 'main'): Promise<string> {
  const payload = PREFIX[net][p2sh ? 'p2sh' : 'p2pkh'] + hash;
  const full = payload + (await sha256(await sha256(payload))).slice(0, 8);
  let n = BigInt('0x' + full);
  let s = '';
  while (n > 0n) {
    s = B58[Number(n % 58n)] + s;
    n /= 58n;
  }
  return s;
}

/** Zatoshis as ZEC with all 8 decimals, as the demo script prints it. */
export const zec = (zat: bigint) => `${zat / 100000000n}.${(zat % 100000000n).toString().padStart(8, '0')}`;
