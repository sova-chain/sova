// Trigger conditions, evaluated on one feed item (SIP-7 §4.2 summary).
// Pure functions: no I/O, so they are tested exhaustively.
//
// Amounts are zatoshis as decimal strings in the feed and in the config,
// compared as BigInt. Pool delta sign: + = value into the pool (SIP-7 §2).

export const POOLS = ['transparent', 'sprout', 'sapling', 'orchard', 'lockbox', 'ironwood'] as const;
export type PoolName = (typeof POOLS)[number];
/** A pool id, or "shielded" = sprout + sapling + orchard + ironwood (as ZcashLib.shieldedTotal). */
export type PoolRef = PoolName | 'shielded';
const SHIELDED: PoolName[] = ['sprout', 'sapling', 'orchard', 'ironwood'];

export const STATS = [
  'txCount', 'shieldedTxCount', 'tIn', 'tOut', 'saplingSpends', 'saplingOutputs',
  'orchardActions', 'ironwoodActions', 'joinSplits',
] as const;
export type StatName = (typeof STATS)[number];

/** One item of sova_getZcashBlocks / sova_subscribe("zcashBlocks"). */
export interface ZcashBlock {
  height: number;
  hash: string;
  time: number;
  sovaBlock: number;
  pools: Record<PoolName, string>;
  chainSupply: string;
  deltas: Record<PoolName, string>;
  stats: Record<StatName, number>;
  trees: { sapling: number; orchard: number; ironwood: number };
}

export type Condition =
  | { kind: 'poolCrossedAbove'; pool: PoolRef; zat: string }
  | { kind: 'poolCrossedBelow'; pool: PoolRef; zat: string }
  | { kind: 'netOutflowAbove'; pool: PoolRef; zat: string }
  | { kind: 'netInflowAbove'; pool: PoolRef; zat: string }
  | { kind: 'statAtLeast'; stat: StatName; value: number };

const pick = (rec: Record<PoolName, string>, pool: PoolRef): bigint =>
  (pool === 'shielded' ? SHIELDED : [pool]).reduce((sum, p) => sum + BigInt(rec[p]), 0n);

/** The pool's total after block h, and its change in block h. */
export function poolAt(b: ZcashBlock, pool: PoolRef): { value: bigint; delta: bigint } {
  return { value: pick(b.pools, pool), delta: pick(b.deltas, pool) };
}

/**
 * Does block `b` satisfy `c`? Crossings compare the total after `b` with the
 * total before it (value − delta, which is chainValue(h−1) by the follower's
 * SIP-7 §3 continuity check), so one item is enough, as for a contract's
 * `condition(h)` reading h and h−1.
 */
export function holds(c: Condition, b: ZcashBlock): boolean {
  switch (c.kind) {
    case 'poolCrossedAbove': {
      const { value, delta } = poolAt(b, c.pool);
      const x = BigInt(c.zat);
      return value >= x && value - delta < x;
    }
    case 'poolCrossedBelow': {
      const { value, delta } = poolAt(b, c.pool);
      const x = BigInt(c.zat);
      return value < x && value - delta >= x;
    }
    case 'netOutflowAbove':
      return -poolAt(b, c.pool).delta > BigInt(c.zat);
    case 'netInflowAbove':
      return poolAt(b, c.pool).delta > BigInt(c.zat);
    case 'statAtLeast':
      return b.stats[c.stat] >= c.value;
  }
}

/** Throw on a malformed condition (config load time, not at the first block). */
export function validate(c: Condition): Condition {
  const poolOk = (p: unknown) => p === 'shielded' || (POOLS as readonly unknown[]).includes(p);
  const zatOk = (z: unknown) => typeof z === 'string' && /^[0-9]+$/.test(z);
  switch (c?.kind) {
    case 'poolCrossedAbove':
    case 'poolCrossedBelow':
    case 'netOutflowAbove':
    case 'netInflowAbove':
      if (!poolOk(c.pool)) throw new Error(`${c.kind}: unknown pool ${JSON.stringify(c.pool)}`);
      if (!zatOk(c.zat)) throw new Error(`${c.kind}: zat must be a decimal string of zatoshis`);
      return c;
    case 'statAtLeast':
      if (!(STATS as readonly unknown[]).includes(c.stat)) throw new Error(`statAtLeast: unknown stat ${JSON.stringify(c.stat)}`);
      if (!Number.isInteger(c.value) || c.value < 0) throw new Error('statAtLeast: value must be a non-negative integer');
      return c;
    default:
      throw new Error(`unknown condition kind ${JSON.stringify((c as { kind?: unknown })?.kind)}`);
  }
}
