// Browser wallets (EIP-1193), shared by /ashwings/{mint,market,buy}: find
// the wallet, connect, and put it on Sova at connect time: switch
// (EIP-3326), and add the chain first when the wallet doesn't know it
// (EIP-3085). Wallets report "unknown chain" differently (4902, a -32603
// wrapping 4902, or something else entirely), so any failed switch that
// isn't the user saying no leads to an add. Nothing here holds a key.
import { rpc } from './chain';

export type Eip1193 = {
  request(a: { method: string; params?: unknown[] }): Promise<any>;
  on?(event: string, fn: (...a: any[]) => void): void;
};

/** EIP-3085 AddEthereumChainParameter. */
export type ChainSpec = {
  chainId: string;
  chainName: string;
  nativeCurrency: { name: string; symbol: string; decimals: number };
  rpcUrls: string[];
  blockExplorerUrls?: string[];
};

export type WalletErrorKind = 'no-wallet' | 'rejected' | 'pending' | 'wrong-chain' | 'failed';
export class WalletError extends Error {
  constructor(readonly kind: WalletErrorKind, message: string) {
    super(message);
  }
}

// ---- finding the wallet ------------------------------------------------------

let found: Eip1193 | undefined;
/**
 * The page's wallet: window.ethereum, else the first EIP-6963 announcement
 * (wallets that leave window.ethereum alone). Waits briefly for late
 * announcements; undefined if there is none.
 */
export async function findWallet(waitMs = 350): Promise<Eip1193 | undefined> {
  if (found) return found;
  const w = window as any;
  if (w.ethereum) return (found = w.ethereum as Eip1193);
  const got: Eip1193[] = [];
  const on = (e: Event) => {
    const p = (e as CustomEvent).detail?.provider;
    if (p) got.push(p);
  };
  window.addEventListener('eip6963:announceProvider', on);
  window.dispatchEvent(new Event('eip6963:requestProvider'));
  await new Promise((r) => setTimeout(r, waitMs));
  window.removeEventListener('eip6963:announceProvider', on);
  return (found = w.ethereum || got[0]);
}

// ---- errors -------------------------------------------------------------------

/** Every error code a wallet put on `e`, outermost first. */
function codes(e: any): number[] {
  const out: number[] = [];
  for (const c of [e?.code, e?.data?.originalError?.code, e?.data?.code, e?.error?.code, e?.cause?.code, e?.data?.cause?.code]) {
    const n = typeof c === 'string' && /^-?\d+$/.test(c) ? Number(c) : c;
    if (typeof n === 'number') out.push(n);
  }
  return out;
}
const msgOf = (e: any) => String(e?.message || e?.data?.message || e || '');
export const isRejected = (e: unknown) =>
  codes(e).includes(4001) || /user (rejected|denied|cancel)|rejected by (the )?user|request rejected/i.test(msgOf(e));
const isPending = (e: unknown) => codes(e).includes(-32002) || /already pending/i.test(msgOf(e));

/** Map a wallet error to a WalletError the page can explain. */
function wrap(e: unknown, what: string): WalletError {
  if (e instanceof WalletError) return e;
  if (isRejected(e)) return new WalletError('rejected', 'rejected in wallet');
  if (isPending(e)) return new WalletError('pending', 'your wallet has a request open: finish it there');
  return new WalletError('failed', `${what}: ${msgOf(e).slice(0, 120) || 'wallet error'}`);
}

// ---- the chain ---------------------------------------------------------------------

export type ChainCfg = { rpc: string; chainId?: number; explorer?: string };
const specs = new Map<string, Promise<ChainSpec>>();
/**
 * The chain `c.rpc` serves, as a wallet adds it. The public testnet
 * (`c.chainId`, 82330 from the deploy record) is "Sova testnet" with the
 * public RPC and explorer; any other chain (a ?rpc= override) is added
 * under its own RPC URL. The currency is SOVA either way.
 */
export function chainSpec(c: ChainCfg): Promise<ChainSpec> {
  let p = specs.get(c.rpc);
  if (!p) {
    p = rpc<string>(c.rpc, 'eth_chainId').then((id) => {
      const testnet = c.chainId !== undefined && BigInt(id) === BigInt(c.chainId);
      return {
        chainId: '0x' + BigInt(id).toString(16),
        chainName: testnet ? 'Sova testnet' : `Sova (${new URL(c.rpc).host})`,
        nativeCurrency: { name: 'SOVA', symbol: 'SOVA', decimals: 18 },
        rpcUrls: [c.rpc],
        ...(testnet && c.explorer ? { blockExplorerUrls: [c.explorer] } : {}),
      };
    });
    p.catch(() => specs.delete(c.rpc));
    specs.set(c.rpc, p);
  }
  return p;
}

/** The wallet's current chain id as hex, or '' if it won't say. */
export async function walletChain(eth: Eip1193): Promise<string> {
  try {
    return '0x' + BigInt(await eth.request({ method: 'eth_chainId' })).toString(16);
  } catch {
    return '';
  }
}

async function onChain(eth: Eip1193, want: string, tries = 1): Promise<boolean> {
  for (let i = 0; i < tries; i++) {
    if ((await walletChain(eth)) === want) return true;
    if (i + 1 < tries) await new Promise((r) => setTimeout(r, 250));
  }
  return false;
}

/**
 * Put the wallet on `spec`'s chain: switch; if the wallet doesn't know the
 * chain (or fails the switch any other way than a "no"), add it, then
 * switch again if the add didn't. Throws a WalletError: 'rejected' when the
 * user said no, 'wrong-chain' if the wallet still isn't there.
 */
export async function ensureChain(eth: Eip1193, spec: ChainSpec): Promise<void> {
  if (await onChain(eth, spec.chainId)) return;
  try {
    await eth.request({ method: 'wallet_switchEthereumChain', params: [{ chainId: spec.chainId }] });
  } catch (e) {
    if (isRejected(e) || isPending(e)) throw wrap(e, 'switch');
    try {
      await eth.request({ method: 'wallet_addEthereumChain', params: [spec] });
    } catch (e2) {
      throw wrap(e2, 'your wallet could not add Sova testnet');
    }
  }
  if (await onChain(eth, spec.chainId, 3)) return;
  // Added but not switched (some wallets do only one per request).
  try {
    await eth.request({ method: 'wallet_switchEthereumChain', params: [{ chainId: spec.chainId }] });
  } catch (e) {
    throw wrap(e, 'switch');
  }
  if (!(await onChain(eth, spec.chainId, 3))) throw new WalletError('wrong-chain', `switch your wallet to ${spec.chainName}`);
}

/** Ask for an account, then put the wallet on the chain. Returns the account. */
export async function connectWallet(eth: Eip1193 | undefined, spec: ChainSpec): Promise<string> {
  if (!eth) throw new WalletError('no-wallet', 'no wallet in this browser');
  let accounts: string[];
  try {
    accounts = await eth.request({ method: 'eth_requestAccounts' });
  } catch (e) {
    throw wrap(e, 'connect');
  }
  if (!accounts?.[0]) throw new WalletError('failed', 'the wallet shared no account');
  await ensureChain(eth, spec);
  return accounts[0];
}

/** The account the site may already use (no prompt), or ''. */
export async function knownAccount(eth: Eip1193): Promise<string> {
  try {
    return ((await eth.request({ method: 'eth_accounts' })) as string[])?.[0] || '';
  } catch {
    return '';
  }
}

/** Follow the wallet: chainChanged (hex id) and accountsChanged (first account or ''). */
export function watchWallet(eth: Eip1193, on: { chain?: (id: string) => void; account?: (a: string) => void }) {
  eth.on?.('chainChanged', (id: unknown) => {
    try {
      on.chain?.('0x' + BigInt(id as string).toString(16));
    } catch {
      on.chain?.('');
    }
  });
  eth.on?.('accountsChanged', (a: unknown) => on.account?.((Array.isArray(a) && a[0]) || ''));
}

/** Phone browsers with no wallet: open this page inside a wallet app instead. */
export function walletAppLinks(href = location.href): { label: string; href: string }[] {
  const bare = href.replace(/^https?:\/\//, '');
  return [
    { label: 'MetaMask', href: `https://metamask.app.link/dapp/${bare}` },
    { label: 'Coinbase Wallet', href: `https://go.cb-w.com/dapp?cb_url=${encodeURIComponent(href)}` },
  ];
}
