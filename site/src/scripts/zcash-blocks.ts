// The ZcashBlocks system contract (SIP-7, 0x…5A01): Sova state records every
// anchored Zcash block. Shared by /pulse and the homepage testnet stats.
import { rpc, words, num } from './checkout/chain';

export const ZCASH_BLOCKS = '0x0000000000000000000000000000000000005a01';

// Selectors (`cast sig`), ZcashBlocks.sol.
export const ZB_SEL = { latest: '52bfe789', summaries: '296f5550' };

/** eth_call against ZcashBlocks at the latest Sova block. */
export const zbCall = (url: string, data: string, at = ZCASH_BLOCKS) =>
  rpc<string>(url, 'eth_call', [{ to: at, data }, 'latest']);

/** latest(): the newest anchored Zcash height (0 = none yet). One eth_call. */
export async function latestAnchored(url: string, at = ZCASH_BLOCKS): Promise<number> {
  return Number(num(words(await zbCall(url, '0x' + ZB_SEL.latest, at))[0]));
}
