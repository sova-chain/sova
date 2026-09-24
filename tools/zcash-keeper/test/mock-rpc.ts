// A mock Sova node for the keeper tests: sova_getZcashBlocks over a chain
// the test edits, the 0x…5A00 anchor(), a ZcashTrigger's ready/minConf,
// and just enough of eth_* to sign and send a transaction.

import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { encodeAbiParameters, keccak256, toFunctionSelector, type Hex } from 'viem';
import type { ZcashBlock } from '../src/triggers.ts';

const READY = toFunctionSelector('ready(uint64)');
const MIN_CONF = toFunctionSelector('minConf()');

export interface MockNode {
  url: string;
  /** Zcash blocks by height (the canonical anchored chain). */
  blocks: Map<number, ZcashBlock>;
  /** Highest anchored height (sova_getZcashBlocks stops here). */
  top: number;
  minConf: number;
  /** ready(h) answer; default: true. */
  ready: (h: number) => boolean;
  calls: { method: string; params: unknown[] }[];
  sent: Hex[];
  close(): Promise<void>;
}

/** A feed item; `ironwood` total and delta in zatoshis, `branch` tags the hash. */
export function block(height: number, ironwood: bigint, delta: bigint, branch = 'a'): ZcashBlock {
  const z = (v: bigint) => v.toString();
  const zero = { transparent: '0', sprout: '0', sapling: '0', orchard: '0', lockbox: '0' };
  return {
    height,
    hash: `0x${branch.repeat(2)}${height.toString(16).padStart(62, '0')}`,
    time: 1_790_000_000 + height,
    sovaBlock: height,
    pools: { ...zero, ironwood: z(ironwood) },
    chainSupply: z(ironwood),
    deltas: { ...zero, ironwood: z(delta) },
    stats: { txCount: 1, shieldedTxCount: 1, tIn: 0, tOut: 1, saplingSpends: 0, saplingOutputs: 0, orchardActions: 0, ironwoodActions: 2, joinSplits: 0 },
    trees: { sapling: 0, orchard: 0, ironwood: height },
  };
}

/** Ironwood totals `values[i]` at heights `from + i` (deltas from the previous total). */
export function chain(from: number, values: bigint[], branch = 'a', before = 0n): ZcashBlock[] {
  return values.map((v, i) => block(from + i, v, v - (i === 0 ? before : values[i - 1]), branch));
}

export async function mockNode(): Promise<MockNode> {
  const node = {
    blocks: new Map<number, ZcashBlock>(),
    top: 0,
    minConf: 1,
    ready: (_h: number) => true,
    calls: [] as { method: string; params: unknown[] }[],
    sent: [] as Hex[],
  } as MockNode;
  const answer = (method: string, params: any[]): unknown => {
    switch (method) {
      case 'sova_getZcashBlocks': {
        const [from, to] = params as [number, number];
        if (to - from >= 1000) throw { code: -32602, message: 'at most 1000 per call' };
        const out = [];
        // Like the node: start at the epoch base (here, the lowest block).
        const base = node.blocks.size ? Math.min(...node.blocks.keys()) : 0;
        for (let h = Math.max(from, base); h <= Math.min(to, node.top); h++) {
          const b = node.blocks.get(h);
          if (!b) break;
          out.push(b);
        }
        return out;
      }
      case 'eth_call': {
        const { to, data } = params[0] as { to: string; data: Hex };
        if (to.toLowerCase().endsWith('5a00')) {
          return encodeAbiParameters([{ type: 'uint64' }, { type: 'bytes32' }], [BigInt(node.top), `0x${'00'.repeat(32)}`]);
        }
        if (data.startsWith(MIN_CONF)) return encodeAbiParameters([{ type: 'uint64' }], [BigInt(node.minConf)]);
        if (data.startsWith(READY)) {
          const h = Number(BigInt(`0x${data.slice(10)}`));
          return encodeAbiParameters([{ type: 'bool' }], [node.ready(h)]);
        }
        throw { code: 3, message: 'execution reverted' };
      }
      case 'eth_chainId': return '0x539';
      case 'eth_getTransactionCount': return '0x7';
      case 'eth_estimateGas': return '0x186a0';
      case 'eth_gasPrice': return '0x3b9aca00';
      case 'eth_maxPriorityFeePerGas': return '0x1';
      case 'eth_sendRawTransaction': node.sent.push(params[0]); return keccak256(params[0]);
      case 'eth_getTransactionReceipt': return { status: '0x1', blockNumber: '0x2a' };
      default: throw { code: -32601, message: `method ${method} not found` };
    }
  };
  const server: Server = createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      const { id, method, params } = JSON.parse(body);
      node.calls.push({ method, params });
      let reply;
      try {
        reply = { jsonrpc: '2.0', id, result: answer(method, params) };
      } catch (error) {
        reply = { jsonrpc: '2.0', id, error };
      }
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify(reply));
    });
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  node.url = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  node.close = () => new Promise<void>((r) => server.close(() => r()));
  return node;
}

export function load(node: MockNode, blocks: ZcashBlock[], top?: number) {
  for (const b of blocks) node.blocks.set(b.height, b);
  node.top = top ?? Math.max(node.top, ...blocks.map((b) => b.height));
}
