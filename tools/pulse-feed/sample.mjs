#!/usr/bin/env node
// Capture real Zcash block summaries (SIP-7 shape) from a zebrad, read-only.
//
//   node sample.mjs [fromHeight] [count]      # default: the last 240 blocks
//   ZEBRAD=http://127.0.0.1:18234 node sample.mjs 4384127 240 > samples.json
//
// One `getblock <h> 2` per block (verbosity 2 carries every tx), parsed the
// way SIP-7 §3 says the follower must: `valuePools` exactly the six known
// ids in order, integer `…Zat` fields only, omitted `trees.<pool>` = 0,
// omitted tx `ironwood` = 0. Each block is cross-checked (§3): pool delta =
// chainValue(h) - chainValue(h-1), and for the four shielded pools the sum
// of tx deltas (-valueBalance) equals the block delta. Output is the exact
// field set of ZcashBlocks.record(Block).
const RPC = process.env.ZEBRAD || 'http://127.0.0.1:18234';
const POOLS = ['transparent', 'sprout', 'sapling', 'orchard', 'lockbox', 'ironwood'];

let id = 0;
async function rpc(method, params = []) {
  const r = await fetch(RPC, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: ++id, method, params }),
  });
  const j = await r.json();
  if (j.error) throw new Error(`${method}: ${j.error.message}`);
  return j.result;
}

const n = (x) => (Array.isArray(x) ? x.length : Number(x || 0));

function summarize(b) {
  const vp = b.valuePools;
  if (!Array.isArray(vp) || vp.length !== 6) throw new Error(`${b.height}: valuePools missing or not 6`);
  vp.forEach((p, i) => {
    if (p.id !== POOLS[i]) throw new Error(`${b.height}: pool ${i} is ${p.id}`);
    if (!Number.isInteger(p.chainValueZat) || !Number.isInteger(p.valueDeltaZat)) throw new Error(`${b.height}: non-integer zat`);
  });
  const s = {
    height: b.height,
    hash: '0x' + b.hash,
    time: b.time,
    txCount: b.tx.length,
    shieldedTxCount: 0,
    tIn: 0,
    tOut: 0,
    saplingSpends: 0,
    saplingOutputs: 0,
    orchardActions: 0,
    ironwoodActions: 0,
    joinSplits: 0,
    pools: vp.map((p) => p.chainValueZat),
    deltas: vp.map((p) => p.valueDeltaZat),
    notes: ['sapling', 'orchard', 'ironwood'].map((k) => b.trees?.[k]?.size ?? 0),
  };
  const txDelta = [0, 0, 0, 0]; // sprout, sapling, orchard, ironwood (pool delta)
  for (const t of b.tx) {
    const sp = n(t.vShieldedSpend), so = n(t.vShieldedOutput), js = n(t.vjoinsplit);
    const oa = n(t.orchard?.actions), ia = n(t.ironwood?.actions);
    // Transparent inputs: the coinbase input is not a spend, so it is not counted.
    s.tIn += (t.vin || []).filter((v) => !v.coinbase).length;
    s.tOut += n(t.vout);
    s.saplingSpends += sp;
    s.saplingOutputs += so;
    s.orchardActions += oa;
    s.ironwoodActions += ia;
    s.joinSplits += js;
    if (sp + so + js + oa + ia > 0) s.shieldedTxCount++;
    for (const j of t.vjoinsplit || []) txDelta[0] += (j.vpub_oldZat ?? 0) - (j.vpub_newZat ?? 0);
    txDelta[1] -= t.valueBalanceZat ?? 0;
    txDelta[2] -= t.orchard?.valueBalanceZat ?? 0;
    txDelta[3] -= t.ironwood?.valueBalanceZat ?? 0;
  }
  [1, 2, 3, 5].forEach((pool, i) => {
    if (txDelta[i] !== s.deltas[pool]) throw new Error(`${b.height}: ${POOLS[pool]} tx deltas ${txDelta[i]} != block ${s.deltas[pool]}`);
  });
  return s;
}

const tip = (await rpc('getblockchaininfo')).blocks;
const count = Number(process.argv[3] || 240);
const from = Number(process.argv[2] || tip - count + 1);
const out = [];
for (let h = from; h < from + count; h++) {
  const s = summarize(await rpc('getblock', [String(h), 2]));
  const prev = out[out.length - 1];
  if (prev) s.pools.forEach((v, i) => {
    if (v - prev.pools[i] !== s.deltas[i]) throw new Error(`${h}: ${POOLS[i]} chainValue diff != delta`);
  });
  out.push(s);
  if (process.stderr.isTTY) process.stderr.write(`\r${h}`);
}
process.stderr.write(`\nsampled ${out.length} blocks ${from}..${from + count - 1}, cross-checks ok\n`);
process.stdout.write(JSON.stringify({ source: 'zebrad testnet getblock <h> 2', from, count, blocks: out }) + '\n');
