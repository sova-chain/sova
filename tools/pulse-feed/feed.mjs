#!/usr/bin/env node
// Play the node's SIP-7 pre-block system call on a dev chain (anvil), so
// /pulse and ZcashBlocks consumers can be built before the node side lands.
//
//   anvil &                       # any dev chain with anvil_* methods
//   node feed.mjs                 # RPC=http://127.0.0.1:8545 by default
//
// 1. Puts ZcashBlocks' runtime bytecode at 0x…5A01 with empty storage
//    (anvil_setCode), exactly what the genesis alloc will hold.
// 2. Impersonates SYSTEM_ADDRESS (0xff…fe) and sends record(Block) with the
//    exact calldata the executor will use, one Zcash block per Sova block,
//    from samples.json (real testnet blocks, see sample.mjs). PREFILL of
//    them go in at once for history, then one every INTERVAL_MS. After the
//    samples run out it keeps going with coinbase-only blocks (testnet
//    subsidy split), marked by a synthetic hash, so a demo can run forever.
// 3. Every PUBLISH_EVERY records, calls publish() from anvil account 0 so
//    eth_getLogs sees ZcashBlock events.
//
// Env: RPC, SAMPLES, PREFILL (120), INTERVAL_MS (4000), PUBLISH_EVERY (5),
// COUNT (stop after this many live records; default: never).
// No dependencies (Node >= 20).
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '../..');
export const ZCASH_BLOCKS = '0x0000000000000000000000000000000000005a01';
export const SYSTEM = '0xfffffffffffffffffffffffffffffffffffffffe';
const DEV0 = '0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266'; // anvil account 0 (public dev key)
const SEL = {
  record: '454a0745', // record((uint64,bytes32,uint32,...,uint64[6],int64[6],uint64[3]))
  window: '461645bf', // window()
  publish: '55b2fd61', // publish(uint64,uint64)
};
// Testnet coinbase-only block (SIP-7 §1.3): transparent, lockbox, ironwood.
const COINBASE_DELTAS = [12_500_000, 0, 0, 0, 18_750_000, 125_000_000];

let seq = 0;
export async function rpc(url, method, params = []) {
  const r = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: ++seq, method, params }),
  });
  const j = await r.json();
  if (j.error) throw new Error(`${method}: ${j.error.message}`);
  return j.result;
}

const word = (x) => {
  let v = BigInt(x);
  if (v < 0n) v += 1n << 256n; // sign-extend int64 to 256 bits
  return v.toString(16).padStart(64, '0');
};

/** Exact record(Block) calldata: selector + 27 static words. */
export function recordCalldata(b) {
  const w = [
    b.height, b.hash, b.time,
    b.txCount, b.shieldedTxCount, b.tIn, b.tOut,
    b.saplingSpends, b.saplingOutputs, b.orchardActions, b.ironwoodActions, b.joinSplits,
    ...b.pools, ...b.deltas, ...b.notes,
  ];
  if (w.length !== 27) throw new Error('bad block');
  return '0x' + SEL.record + w.map(word).join('');
}

export function loadSamples(file = join(HERE, 'samples.json')) {
  return JSON.parse(readFileSync(file, 'utf8')).blocks;
}

/** Next coinbase-only block after `b` (used once the samples run out). */
export function synthNext(b) {
  const height = b.height + 1;
  return {
    ...b,
    height,
    hash: '0x' + createHash('sha256').update(`synthetic zcash block ${height}`).digest('hex'),
    time: b.time + 75,
    txCount: 1, shieldedTxCount: 1, tIn: 0, tOut: 1,
    saplingSpends: 0, saplingOutputs: 0, orchardActions: 0, ironwoodActions: 2, joinSplits: 0,
    pools: b.pools.map((v, i) => v + COINBASE_DELTAS[i]),
    deltas: [...COINBASE_DELTAS],
    notes: [b.notes[0], b.notes[1], b.notes[2] + 2],
  };
}

async function receipt(url, hash) {
  for (let i = 0; i < 200; i++) {
    const r = await rpc(url, 'eth_getTransactionReceipt', [hash]);
    if (r) return r;
    await new Promise((ok) => setTimeout(ok, 50));
  }
  throw new Error('no receipt ' + hash);
}

/** Install ZcashBlocks at 0x…5A01 (idempotent) and unlock the system caller. */
export async function install(url) {
  const art = JSON.parse(readFileSync(join(REPO, 'contracts/out/ZcashBlocks.sol/ZcashBlocks.json'), 'utf8'));
  const code = art.deployedBytecode.object;
  const have = await rpc(url, 'eth_getCode', [ZCASH_BLOCKS, 'latest']);
  if (have === '0x' || have.length <= 2) await rpc(url, 'anvil_setCode', [ZCASH_BLOCKS, code]);
  else if (have.toLowerCase() !== code.toLowerCase()) throw new Error('different code already at 0x…5A01');
  await rpc(url, 'anvil_setBalance', [SYSTEM, '0x' + (10n ** 24n).toString(16)]);
  await rpc(url, 'anvil_impersonateAccount', [SYSTEM]);
}

export async function newestRecorded(url) {
  const ret = await rpc(url, 'eth_call', [{ to: ZCASH_BLOCKS, data: '0x' + SEL.window }, 'latest']);
  return Number(BigInt('0x' + ret.slice(66, 130)));
}

/** Send one system-call record; returns the receipt. */
export async function record(url, b) {
  const hash = await rpc(url, 'eth_sendTransaction', [
    { from: SYSTEM, to: ZCASH_BLOCKS, data: recordCalldata(b), gas: '0x1c9c380' },
  ]);
  const r = await receipt(url, hash);
  if (r.status !== '0x1') throw new Error(`record(${b.height}) reverted`);
  return r;
}

export async function publish(url, from, to) {
  const hash = await rpc(url, 'eth_sendTransaction', [
    { from: DEV0, to: ZCASH_BLOCKS, data: '0x' + SEL.publish + word(from) + word(to), gas: '0x1c9c380' },
  ]);
  return receipt(url, hash);
}

/**
 * Run the feed. Resolves after `count` live records (or never).
 * `onRecord({block, receipt, live})` is called after each record.
 */
export async function runFeed({ url, samples, prefill = 120, intervalMs = 4000, publishEvery = 5, count = Infinity, onRecord = () => {}, log = console.log }) {
  await install(url);
  const newest = await newestRecorded(url);
  let i = newest ? samples.findIndex((s) => s.height === newest + 1) : 0;
  let last = newest ? samples.find((s) => s.height === newest) : null;
  if (newest && i < 0 && !last) throw new Error(`chain already at Zcash ${newest}, not in samples`);
  const next = () => (i >= 0 && i < samples.length ? samples[i++] : ((i = -1), synthNext(last)));
  let published = newest;
  const pub = async (to) => {
    if (to <= published) return;
    const r = await publish(url, published ? published + 1 : 0, to);
    log(`publish -> ${r.logs.length} ZcashBlock logs (gas ${Number(r.gasUsed)})`);
    published = to;
  };

  for (let k = 0; !newest && k < prefill; k++) {
    last = next();
    const r = await record(url, last);
    onRecord({ block: last, receipt: r, live: false });
  }
  if (!newest) log(`prefilled ${prefill} Zcash blocks, newest ${last.height}`);
  if (last) await pub(last.height);

  for (let k = 0; k < count; k++) {
    await new Promise((ok) => setTimeout(ok, intervalMs));
    last = next();
    const r = await record(url, last);
    onRecord({ block: last, receipt: r, live: true });
    const sh = last.pools[1] + last.pools[2] + last.pools[3] + last.pools[5];
    log(`zcash ${last.height} -> sova ${Number(r.blockNumber)}  shielded ${(sh / 1e8).toFixed(8)}  gas ${Number(r.gasUsed)}`);
    if ((k + 1) % publishEvery === 0) await pub(last.height);
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const env = process.env;
  await runFeed({
    url: env.RPC || 'http://127.0.0.1:8545',
    samples: loadSamples(env.SAMPLES),
    prefill: Number(env.PREFILL || 120),
    intervalMs: Number(env.INTERVAL_MS || 4000),
    publishEvery: Number(env.PUBLISH_EVERY || 5),
    count: env.COUNT ? Number(env.COUNT) : Infinity,
  });
}
