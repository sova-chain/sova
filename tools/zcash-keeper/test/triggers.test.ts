import assert from 'node:assert/strict';
import { test } from 'node:test';
import { holds, validate, type Condition } from '../src/triggers.ts';
import { block } from './mock-rpc.ts';

const M = 1_000_000n;

test('poolCrossedAbove holds only at the crossing height', () => {
  const c: Condition = { kind: 'poolCrossedAbove', pool: 'ironwood', zat: String(10n * M) };
  assert.equal(holds(c, block(1, 9n * M, 1n * M)), false, 'below');
  assert.equal(holds(c, block(2, 10n * M, 1n * M)), true, 'reaches it exactly');
  assert.equal(holds(c, block(3, 11n * M, 1n * M)), false, 'already above before');
  assert.equal(holds(c, block(4, 12n * M, 3n * M)), true, 'jumps across');
});

test('poolCrossedBelow mirrors it', () => {
  const c: Condition = { kind: 'poolCrossedBelow', pool: 'ironwood', zat: String(10n * M) };
  assert.equal(holds(c, block(1, 9n * M, -1n * M)), true);
  assert.equal(holds(c, block(2, 8n * M, -1n * M)), false);
  assert.equal(holds(c, block(3, 10n * M, -1n * M)), false);
});

test('net flows use the signed delta (+ = into the pool)', () => {
  const out: Condition = { kind: 'netOutflowAbove', pool: 'ironwood', zat: '1000' };
  const inn: Condition = { kind: 'netInflowAbove', pool: 'ironwood', zat: '1000' };
  assert.equal(holds(out, block(1, 0n, -1001n)), true);
  assert.equal(holds(out, block(1, 0n, -1000n)), false, 'strictly above');
  assert.equal(holds(out, block(1, 0n, 5000n)), false);
  assert.equal(holds(inn, block(1, 0n, 5000n)), true);
});

test('"shielded" sums sprout, sapling, orchard and ironwood, not transparent or lockbox', () => {
  const b = block(1, 5n, 5n);
  b.pools = { transparent: '100', sprout: '1', sapling: '2', orchard: '3', lockbox: '100', ironwood: '4' };
  b.deltas = { transparent: '100', sprout: '0', sapling: '2', orchard: '0', lockbox: '100', ironwood: '0' };
  assert.equal(holds({ kind: 'poolCrossedAbove', pool: 'shielded', zat: '10' }, b), true, '8 -> 10');
  assert.equal(holds({ kind: 'poolCrossedAbove', pool: 'shielded', zat: '11' }, b), false);
  assert.equal(holds({ kind: 'netInflowAbove', pool: 'shielded', zat: '1' }, b), true);
});

test('statAtLeast', () => {
  assert.equal(holds({ kind: 'statAtLeast', stat: 'ironwoodActions', value: 2 }, block(1, 0n, 0n)), true);
  assert.equal(holds({ kind: 'statAtLeast', stat: 'ironwoodActions', value: 3 }, block(1, 0n, 0n)), false);
});

test('validate rejects malformed conditions', () => {
  for (const bad of [
    { kind: 'poolCrossedAbove', pool: 'zcash', zat: '1' },
    { kind: 'poolCrossedAbove', pool: 'ironwood', zat: 1 },
    { kind: 'netOutflowAbove', pool: 'ironwood', zat: '-5' },
    { kind: 'statAtLeast', stat: 'txs', value: 1 },
    { kind: 'statAtLeast', stat: 'txCount', value: 1.5 },
    { kind: 'whenever' },
  ]) {
    assert.throws(() => validate(bad as unknown as Condition), Error, JSON.stringify(bad));
  }
  assert.doesNotThrow(() => validate({ kind: 'poolCrossedAbove', pool: 'shielded', zat: '1' }));
});
