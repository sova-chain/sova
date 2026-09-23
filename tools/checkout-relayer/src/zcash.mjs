// Zcash side: a minimal zebrad JSON-RPC client and t-address encoding.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const PREFIX = { test: { p2pkh: '1d25', p2sh: '1cba' }, main: { p2pkh: '1cb8', p2sh: '1cbd' } };
const sha256 = (buf) => createHash('sha256').update(buf).digest();

/** Base58Check t-address of a 20-byte hash (0x-hex). */
export function tAddr(hash, p2sh, net) {
  const payload = Buffer.from(PREFIX[net][p2sh ? 'p2sh' : 'p2pkh'] + hash.replace(/^0x/, ''), 'hex');
  const full = Buffer.concat([payload, sha256(sha256(payload)).subarray(0, 4)]);
  let n = BigInt('0x' + full.toString('hex'));
  let s = '';
  while (n > 0n) {
    s = B58[Number(n % 58n)] + s;
    n /= 58n;
  }
  return s;
}

/** Raw scriptPubKey hex a reservation must be paid to. */
export const payeeScript = (hash, p2sh) => {
  const h = hash.replace(/^0x/, '').toLowerCase();
  return p2sh ? `a914${h}87` : `76a914${h}88ac`;
};

/**
 * zebrad JSON-RPC. Auth: ZCASH_RPC_USER/ZCASH_RPC_PASSWORD, or
 * ZCASH_RPC_COOKIE (zebrad's cookie file, re-read on every call because
 * zebrad rewrites it on restart).
 */
export function zebrad({ url, user, password, cookieFile }) {
  let seq = 0;
  return async function call(method, params = []) {
    const headers = { 'content-type': 'application/json' };
    const cred = cookieFile ? readFileSync(cookieFile, 'utf8').trim() : user ? `${user}:${password ?? ''}` : '';
    if (cred) headers.authorization = 'Basic ' + Buffer.from(cred).toString('base64');
    const res = await fetch(url, {
      method: 'POST', headers, body: JSON.stringify({ jsonrpc: '2.0', id: ++seq, method, params }),
    });
    const j = await res.json();
    if (j.error) throw new Error(`zebrad ${method}: ${j.error.message}`);
    return j.result;
  };
}
