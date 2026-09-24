// The keeper: evaluates configured triggers on each feed item and, when one
// holds at Zcash height h, sends poke(h) to its ZcashTrigger contract
// (contracts/src/zcash/ZcashTrigger.sol, SIP-7 §4.3) once h is deep enough
// for the contract's minConf. The contract re-checks everything; the keeper
// asks it first with ready(h) (eth_call), so a disagreement costs no gas.

import { decodeFunctionResult, encodeFunctionData, type Hex } from 'viem';
import { privateKeyToAccount, type PrivateKeyAccount } from 'viem/accounts';
import type { FeedEvent } from './feed.ts';
import { rpc } from './feed.ts';
import { holds, validate, type Condition } from './triggers.ts';

export const TRIGGER_ABI = [
  { type: 'function', name: 'poke', stateMutability: 'nonpayable', inputs: [{ name: 'h', type: 'uint64' }], outputs: [] },
  { type: 'function', name: 'ready', stateMutability: 'view', inputs: [{ name: 'h', type: 'uint64' }], outputs: [{ type: 'bool' }] },
  { type: 'function', name: 'minConf', stateMutability: 'view', inputs: [], outputs: [{ type: 'uint64' }] },
] as const;

export interface TriggerConfig {
  name: string;
  /** The ZcashTrigger contract to poke. */
  contract: Hex;
  when: Condition;
  /** Keep watching after a successful poke (for repeating triggers). Default false. */
  repeat?: boolean;
}

export interface KeeperOptions {
  rpc: string;
  triggers: TriggerConfig[];
  /** Print what would be sent instead of sending. */
  dryRun: boolean;
  /** Signs poke(h) (required unless dryRun). */
  privateKey?: Hex;
  /** Wait for each poke's receipt (ms; 0 = don't wait). */
  receiptTimeoutMs?: number;
  log?: (line: Record<string, unknown>) => void;
}

interface Witness { trigger: TriggerConfig; height: number }

export class Keeper {
  private readonly opts: KeeperOptions;
  private readonly account?: PrivateKeyAccount;
  private readonly log: (line: Record<string, unknown>) => void;
  private readonly minConfs = new Map<string, number>();
  private readonly done = new Set<string>();
  /** Condition met, waiting for depth. */
  pending: Witness[] = [];
  /** Highest anchored Zcash height seen on the feed. */
  top = -1;

  constructor(opts: KeeperOptions) {
    for (const t of opts.triggers) {
      validate(t.when);
      if (!/^0x[0-9a-fA-F]{40}$/.test(t.contract)) throw new Error(`${t.name}: contract must be a 0x address`);
    }
    if (!opts.dryRun && !opts.privateKey) throw new Error('a private key is required unless dry-run');
    this.opts = opts;
    this.account = opts.privateKey ? privateKeyToAccount(opts.privateKey) : undefined;
    this.log = opts.log ?? ((line) => console.log(JSON.stringify(line)));
  }

  /** Consume one feed event. */
  async onEvent(ev: FeedEvent): Promise<void> {
    if (ev.kind === 'rollback') {
      const dropped = this.pending.filter((w) => w.height > ev.toHeight);
      this.pending = this.pending.filter((w) => w.height <= ev.toHeight);
      this.top = Math.min(this.top, ev.toHeight);
      this.log({ event: 'rollback', toHeight: ev.toHeight, droppedWitnesses: dropped.map((w) => [w.trigger.name, w.height]) });
      return;
    }
    const b = ev.block;
    this.top = b.height;
    for (const trigger of this.opts.triggers) {
      if (this.done.has(trigger.name) || this.pending.some((w) => w.trigger === trigger)) continue;
      if (holds(trigger.when, b)) {
        this.log({ event: 'condition', trigger: trigger.name, height: b.height, hash: b.hash });
        this.pending.push({ trigger, height: b.height });
      }
    }
    await this.flush();
  }

  /**
   * Poke every witness that is now deep enough. A witness whose RPC fails
   * (for example eth_call while the Sova head is stale right after a Zcash
   * reorg) stays pending and is retried on the next block.
   */
  async flush(): Promise<void> {
    for (const w of [...this.pending]) {
      try {
        const minConf = await this.minConf(w.trigger.contract);
        if (w.height + minConf - 1 > this.top) continue;
        this.pending = this.pending.filter((p) => p !== w);
        await this.poke(w, minConf);
      } catch (e) {
        // Retry unless the poke already went out (a later step failed).
        if (!this.done.has(w.trigger.name) && !this.pending.includes(w)) this.pending.push(w);
        this.log({ event: 'retry', trigger: w.trigger.name, height: w.height, message: (e as Error).message });
      }
    }
  }

  private async minConf(contract: Hex): Promise<number> {
    const key = contract.toLowerCase();
    const cached = this.minConfs.get(key);
    if (cached !== undefined) return cached;
    try {
      const n = Number(await this.call(contract, 'minConf', []));
      this.minConfs.set(key, n);
      return n;
    } catch (e) {
      if (!this.opts.dryRun) throw e;
      // Dry-run against an address with no trigger deployed (not cached, so
      // a transient failure doesn't stick).
      this.log({ event: 'warn', contract, message: `minConf() failed (${(e as Error).message}); assuming 1` });
      return 1;
    }
  }

  private async call(contract: Hex, fn: 'ready' | 'minConf', args: [bigint] | []): Promise<unknown> {
    const data = encodeFunctionData({ abi: TRIGGER_ABI, functionName: fn, args } as never);
    const out = await rpc<Hex>(this.opts.rpc, 'eth_call', [{ to: contract, data }, 'latest']);
    return decodeFunctionResult({ abi: TRIGGER_ABI, functionName: fn, data: out } as never);
  }

  private async poke(w: Witness, minConf: number): Promise<void> {
    const { trigger, height } = w;
    const data = encodeFunctionData({ abi: TRIGGER_ABI, functionName: 'poke', args: [BigInt(height)] });
    let ready: boolean | null;
    try {
      ready = Boolean(await this.call(trigger.contract, 'ready', [BigInt(height)]));
    } catch {
      ready = null;
    }
    const line = { trigger: trigger.name, to: trigger.contract, height, minConf, data, ready };
    if (this.opts.dryRun) {
      this.log({ event: 'would-poke', ...line });
      if (!trigger.repeat) this.done.add(trigger.name);
      return;
    }
    if (ready === null) throw new Error(`ready(${height}) call failed`);
    if (!ready) {
      this.log({ event: 'skip', ...line, message: 'contract ready(h) is false; not sending' });
      return;
    }
    const hash = await this.send(trigger.contract, data);
    this.log({ event: 'poked', ...line, tx: hash });
    if (!trigger.repeat) this.done.add(trigger.name);
    const receipt = await this.receipt(hash);
    if (receipt) this.log({ event: 'receipt', tx: hash, status: receipt.status, block: Number(BigInt(receipt.blockNumber)) });
  }

  private async send(to: Hex, data: Hex): Promise<Hex> {
    const account = this.account as PrivateKeyAccount;
    const url = this.opts.rpc;
    const [chainId, nonce, gas, gasPrice, tip] = await Promise.all([
      rpc<Hex>(url, 'eth_chainId'),
      rpc<Hex>(url, 'eth_getTransactionCount', [account.address, 'pending']),
      rpc<Hex>(url, 'eth_estimateGas', [{ from: account.address, to, data }, 'pending']),
      rpc<Hex>(url, 'eth_gasPrice'),
      rpc<Hex>(url, 'eth_maxPriorityFeePerGas'),
    ]);
    const raw = await account.signTransaction({
      type: 'eip1559',
      chainId: Number(BigInt(chainId)),
      nonce: Number(BigInt(nonce)),
      to,
      data,
      gas: (BigInt(gas) * 12n) / 10n,
      maxFeePerGas: BigInt(gasPrice) * 2n,
      maxPriorityFeePerGas: BigInt(tip),
    });
    return rpc<Hex>(url, 'eth_sendRawTransaction', [raw]);
  }

  private async receipt(hash: Hex): Promise<{ status: string; blockNumber: Hex } | null> {
    const deadline = Date.now() + (this.opts.receiptTimeoutMs ?? 60_000);
    while (Date.now() < deadline) {
      const r = await rpc<{ status: string; blockNumber: Hex } | null>(this.opts.rpc, 'eth_getTransactionReceipt', [hash]);
      if (r) return r;
      await new Promise((res) => setTimeout(res, 500));
    }
    return null;
  }
}
