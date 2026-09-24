#!/usr/bin/env node
// Reference SIP-7 keeper (§4.3). See README.md.
//
//   node src/main.ts --config config.json [--dry-run] [--ws ws://…] [--from H]
//
// Key: KEEPER_KEY (0x-hex), or --dev-key for the public reth/anvil dev key
// (chain ID 1337 only). Not needed with --dry-run.

import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import type { Hex } from 'viem';
import { anchoredHeight, pollFeed, rpc, wsFeed, type FeedEvent } from './feed.ts';
import { Keeper, type TriggerConfig } from './keeper.ts';

/** reth --dev / anvil account 0. Public; only for local dev chains. */
export const DEV_KEY: Hex = '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80';
const DEV_CHAIN_ID = 1337;

export interface RunOptions {
  rpc: string;
  ws?: string;
  triggers: TriggerConfig[];
  dryRun: boolean;
  privateKey?: Hex;
  /** First Zcash height to evaluate; default: `lookback` below the current anchored height. */
  from?: number;
  lookback?: number;
  pollMs?: number;
  /** Stop after this many blocks (demos, tests). */
  maxBlocks?: number;
  signal?: AbortSignal;
  log?: (line: Record<string, unknown>) => void;
}

export async function run(o: RunOptions): Promise<Keeper> {
  const log = o.log ?? ((line) => console.log(JSON.stringify(line)));
  const keeper = new Keeper({ rpc: o.rpc, triggers: o.triggers, dryRun: o.dryRun, privateKey: o.privateKey, log });
  const from = o.from ?? Math.max(0, (await anchoredHeight(o.rpc)) - (o.lookback ?? 0) + 1);
  log({ event: 'start', rpc: o.rpc, source: o.ws ? `ws ${o.ws}` : 'poll sova_getZcashBlocks', from, dryRun: o.dryRun, triggers: o.triggers.map((t) => t.name) });
  const feed: AsyncGenerator<FeedEvent> = o.ws
    ? wsFeed(o.ws, { from, signal: o.signal })
    : pollFeed(o.rpc, { from, intervalMs: o.pollMs ?? 1000, signal: o.signal, onError: (e) => log({ event: 'warn', message: e.message }) });
  let blocks = 0;
  for await (const ev of feed) {
    if (ev.kind === 'block') log({ event: 'block', height: ev.block.height, hash: ev.block.hash, sovaBlock: ev.block.sovaBlock });
    await keeper.onEvent(ev);
    if (ev.kind === 'block' && o.maxBlocks !== undefined && ++blocks >= o.maxBlocks) break;
  }
  return keeper;
}

async function main() {
  const { values } = parseArgs({
    options: {
      config: { type: 'string' },
      rpc: { type: 'string' },
      ws: { type: 'string' },
      from: { type: 'string' },
      lookback: { type: 'string' },
      'max-blocks': { type: 'string' },
      'dry-run': { type: 'boolean', default: false },
      'dev-key': { type: 'boolean', default: false },
    },
  });
  if (!values.config) throw new Error('usage: node src/main.ts --config config.json [--dry-run] [--rpc URL] [--ws URL] [--from H] [--lookback N] [--max-blocks N] [--dev-key]');
  const cfg = JSON.parse(readFileSync(values.config, 'utf8')) as { rpc?: string; ws?: string; triggers: TriggerConfig[] };
  const url = values.rpc ?? cfg.rpc ?? 'http://127.0.0.1:8545';
  let privateKey = process.env.KEEPER_KEY as Hex | undefined;
  if (values['dev-key']) {
    const chainId = Number(BigInt(await rpc<string>(url, 'eth_chainId')));
    if (chainId !== DEV_CHAIN_ID) throw new Error(`--dev-key is only for the dev chain (ID ${DEV_CHAIN_ID}); this RPC is chain ${chainId}`);
    privateKey = DEV_KEY;
  }
  const int = (s?: string) => (s === undefined ? undefined : Number.parseInt(s, 10));
  const ac = new AbortController();
  process.on('SIGINT', () => ac.abort());
  process.on('SIGTERM', () => ac.abort());
  await run({
    rpc: url,
    ws: values.ws ?? cfg.ws,
    triggers: cfg.triggers,
    dryRun: values['dry-run'],
    privateKey,
    from: int(values.from),
    lookback: int(values.lookback),
    maxBlocks: int(values['max-blocks']),
    signal: ac.signal,
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((e) => {
    console.error(String(e?.message ?? e));
    process.exit(1);
  });
}
