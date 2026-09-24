import assert from 'node:assert/strict';
import { after, before, beforeEach, test } from 'node:test';
import { encodeFunctionData, parseTransaction, recoverTransactionAddress, type Hex } from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import { pollFeed, type FeedEvent } from '../src/feed.ts';
import { Keeper, TRIGGER_ABI, type TriggerConfig } from '../src/keeper.ts';
import { DEV_KEY, run } from '../src/main.ts';
import { chain, load, mockNode, type MockNode } from './mock-rpc.ts';

const M = 1_000_000n;
const CONTRACT: Hex = '0x00000000000000000000000000000000000c0ffe';
const IRONWOOD_10M: TriggerConfig = {
  name: 'ironwood-10M',
  contract: CONTRACT,
  when: { kind: 'poolCrossedAbove', pool: 'ironwood', zat: String(10n * M) },
};
const pokeData = (h: number) => encodeFunctionData({ abi: TRIGGER_ABI, functionName: 'poke', args: [BigInt(h)] });

let node: MockNode;
let lines: Record<string, any>[];
const log = (l: Record<string, unknown>) => { lines.push(l); };
const events = (kind: string) => lines.filter((l) => l.event === kind);

before(async () => { node = await mockNode(); });
after(async () => { await node.close(); });
beforeEach(() => {
  node.blocks.clear();
  node.top = 0;
  node.minConf = 1;
  node.ready = () => true;
  node.calls.length = 0;
  node.sent.length = 0;
  lines = [];
});

// Ironwood 6M, 8M, 11M, 12M, … at heights 1, 2, 3, 4, …: crosses 10M at 3.
const RISING = [6n, 8n, 11n, 12n, 13n, 14n].map((v) => v * M);

test('dry-run: prints the poke once the witness has minConf, sends nothing', async () => {
  load(node, chain(1, RISING));
  node.minConf = 3;
  await run({ rpc: node.url, triggers: [IRONWOOD_10M], dryRun: true, from: 1, maxBlocks: 6, pollMs: 10, log });
  assert.deepEqual(events('condition').map((l) => l.height), [3]);
  const [would] = events('would-poke');
  assert.deepEqual(
    { height: would.height, to: would.to, data: would.data, ready: would.ready, minConf: would.minConf },
    { height: 3, to: CONTRACT, data: pokeData(3), ready: true, minConf: 3 },
  );
  // Printed when height 5 (= 3 + minConf − 1) arrived, not before.
  const idx = (pred: (l: any) => boolean) => lines.findIndex(pred);
  assert.ok(idx((l) => l.event === 'would-poke') > idx((l) => l.event === 'block' && l.height === 5));
  assert.ok(idx((l) => l.event === 'would-poke') < idx((l) => l.event === 'block' && l.height === 6));
  assert.equal(events('would-poke').length, 1, 'one-shot');
  assert.equal(node.sent.length, 0);
  assert.ok(!node.calls.some((c) => c.method === 'eth_sendRawTransaction'));
});

test('live: signs poke(h) with the key and sends it to the trigger', async () => {
  load(node, chain(1, RISING));
  await run({ rpc: node.url, triggers: [IRONWOOD_10M], dryRun: false, privateKey: DEV_KEY, from: 1, maxBlocks: 6, pollMs: 10, log });
  assert.equal(node.sent.length, 1);
  const raw = node.sent[0];
  const tx = parseTransaction(raw);
  assert.equal(tx.to, CONTRACT);
  assert.equal(tx.data, pokeData(3));
  assert.equal(tx.chainId, 1337);
  assert.equal(tx.nonce, 7);
  assert.equal(await recoverTransactionAddress({ serializedTransaction: raw as any }), privateKeyToAccount(DEV_KEY).address);
  assert.equal(events('poked')[0].height, 3);
  assert.equal(events('receipt')[0].status, '0x1');
});

test('live: a contract that says not ready(h) gets no transaction', async () => {
  load(node, chain(1, RISING));
  node.ready = () => false;
  await run({ rpc: node.url, triggers: [IRONWOOD_10M], dryRun: false, privateKey: DEV_KEY, from: 1, maxBlocks: 6, pollMs: 10, log });
  assert.equal(node.sent.length, 0);
  assert.equal(events('skip')[0].height, 3);
});

test('a key is required outside dry-run; bad configs are refused', () => {
  assert.throws(() => new Keeper({ rpc: node.url, triggers: [IRONWOOD_10M], dryRun: false }), /private key/);
  assert.throws(() => new Keeper({ rpc: node.url, triggers: [{ ...IRONWOOD_10M, contract: '0x12' as Hex }], dryRun: true }), /address/);
  assert.throws(() => new Keeper({ rpc: node.url, triggers: [{ ...IRONWOOD_10M, when: { kind: 'nope' } as any }], dryRun: true }), /unknown condition/);
});

/** Pull `n` events from a feed. */
async function take(feed: AsyncGenerator<FeedEvent>, n: number): Promise<FeedEvent[]> {
  const out: FeedEvent[] = [];
  while (out.length < n) {
    const { value, done } = await feed.next();
    if (done) break;
    out.push(value);
  }
  return out;
}
const summary = (evs: FeedEvent[]) => evs.map((e) => (e.kind === 'block' ? `${e.block.height}${e.block.hash.slice(2, 3)}` : `rollback:${e.toHeight}`));

test('poller: streams in order, then reports a reorg as rollback to the fork + new branch', async () => {
  load(node, chain(1, RISING.slice(0, 4)));
  const ac = new AbortController();
  const feed = pollFeed(node.url, { from: 1, intervalMs: 5, signal: ac.signal });
  assert.deepEqual(summary(await take(feed, 4)), ['1a', '2a', '3a', '4a']);
  // Zcash reorg from height 3: branch b, one block longer.
  load(node, chain(3, [9n * M, 10n * M, 11n * M], 'b', 8n * M));
  assert.deepEqual(summary(await take(feed, 4)), ['rollback:2', '3b', '4b', '5b']);
  // The anchored height drops (Sova unwound) with the same hashes: rollback
  // to the new top, then the same blocks again as it regrows.
  node.top = 3;
  assert.deepEqual(summary(await take(feed, 1)), ['rollback:3']);
  node.top = 5;
  assert.deepEqual(summary(await take(feed, 2)), ['4b', '5b']);
  ac.abort();
});

test('poller: a reorg below everything it remembers rolls back to the anchored top', async () => {
  load(node, chain(1, RISING.slice(0, 4)));
  const ac = new AbortController();
  const feed = pollFeed(node.url, { from: 4, intervalMs: 5, signal: ac.signal });
  assert.deepEqual(summary(await take(feed, 1)), ['4a']);
  // Zcash unwinds to 2 (nothing new yet), then a longer branch c from 3.
  node.top = 2;
  assert.deepEqual(summary(await take(feed, 1)), ['rollback:2']);
  load(node, chain(3, [9n * M, 10n * M, 11n * M], 'c', 8n * M));
  assert.deepEqual(summary(await take(feed, 3)), ['3c', '4c', '5c']);
  ac.abort();
});

test('keeper: a rollback drops a witness that was waiting for depth; the new branch can re-arm it', async () => {
  node.minConf = 3;
  const k = new Keeper({ rpc: node.url, triggers: [IRONWOOD_10M], dryRun: true, log });
  const blocks = chain(1, RISING);
  for (const b of blocks.slice(0, 4)) await k.onEvent({ kind: 'block', block: b }); // crosses at 3, top 4
  assert.deepEqual(k.pending.map((w) => w.height), [3]);
  await k.onEvent({ kind: 'rollback', toHeight: 2 });
  assert.deepEqual(k.pending, []);
  // New branch crosses at 4 instead.
  const b2 = chain(3, [9n * M, 10n * M, 11n * M, 12n * M], 'b', 8n * M);
  for (const b of b2) await k.onEvent({ kind: 'block', block: b });
  assert.deepEqual(events('would-poke').map((l) => l.height), [4]);
  assert.deepEqual(events('rollback')[0].droppedWitnesses, [['ironwood-10M', 3]]);
});

test('default start: the anchored height from 0x…5A00 anchor(), minus lookback', async () => {
  load(node, chain(1, RISING));
  await run({ rpc: node.url, triggers: [], dryRun: true, lookback: 2, maxBlocks: 2, pollMs: 10, log });
  assert.equal(events('start')[0].from, 5);
  assert.deepEqual(events('block').map((l) => l.height), [5, 6]);
});

test('live: a failed RPC keeps the witness pending and retries it on the next block', async () => {
  load(node, chain(1, RISING));
  let failures = 1;
  node.ready = () => {
    if (failures-- > 0) throw { code: -32603, message: 'zcash index does not cover anchored height' };
    return true;
  };
  await run({ rpc: node.url, triggers: [IRONWOOD_10M], dryRun: false, privateKey: DEV_KEY, from: 1, maxBlocks: 6, pollMs: 10, log });
  assert.deepEqual(events('retry').map((l) => l.height), [3]);
  assert.equal(node.sent.length, 1, 'sent once, on the retry');
  assert.equal(parseTransaction(node.sent[0]).data, pokeData(3));
});
