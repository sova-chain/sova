// The node's SIP-7 §4.2 feed, as an async stream of events: by polling
// sova_getZcashBlocks over HTTP, or by sova_subscribe("zcashBlocks") over
// WS. Both yield the same events, so the keeper doesn't care which.

import type { ZcashBlock } from './triggers.ts';

export type FeedEvent =
  | { kind: 'block'; block: ZcashBlock }
  | { kind: 'rollback'; toHeight: number };

/** The node's cap on one sova_getZcashBlocks call. */
export const MAX_RANGE = 1000;
/** How far back the poller looks for a fork after a reorg. */
const FORK_WINDOW = 256;
const ZCASH_PRECOMPILE = '0x0000000000000000000000000000000000005a00';
const ANCHOR_SELECTOR = '0xd3fb73b4'; // anchor() -> (uint64 height, bytes32 hash)

let seq = 0;
export async function rpc<T = unknown>(url: string, method: string, params: unknown[] = []): Promise<T> {
  const r = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: ++seq, method, params }),
  });
  const j = (await r.json()) as { result?: T; error?: { code: number; message: string } };
  if (j.error) throw new Error(`${method}: ${j.error.message} (${j.error.code})`);
  return j.result as T;
}

/** The Zcash height the latest Sova block anchors (SIP-4 `anchor()` on 0x…5A00). */
export async function anchoredHeight(url: string): Promise<number> {
  const out = await rpc<string>(url, 'eth_call', [{ to: ZCASH_PRECOMPILE, data: ANCHOR_SELECTOR }, 'latest']);
  return Number(BigInt(out.slice(0, 66)));
}

const sleep = (ms: number, signal?: AbortSignal) =>
  new Promise<void>((resolve) => {
    const t = setTimeout(resolve, ms);
    signal?.addEventListener('abort', () => { clearTimeout(t); resolve(); }, { once: true });
  });

/**
 * Poll sova_getZcashBlocks from `from`. Every round re-reads the last height
 * it delivered: if that height is gone or its hash changed, the node's
 * canonical chain moved under us, so it finds the fork (the highest height
 * whose hash still matches) and yields a rollback before the new items.
 */
export async function* pollFeed(
  url: string,
  opts: { from: number; intervalMs: number; signal?: AbortSignal; onError?: (e: Error) => void },
): AsyncGenerator<FeedEvent> {
  const seen = new Map<number, string>();
  let next = opts.from;
  let first = true;
  while (!opts.signal?.aborted) {
    const last = next - 1;
    const lo = seen.has(last) ? last : next;
    let items: ZcashBlock[];
    let toHeight: number | null = null;
    try {
      items = await rpc<ZcashBlock[]>(url, 'sova_getZcashBlocks', [lo, lo + MAX_RANGE - 1]);
      if (lo === last) {
        const head = items.shift();
        if (!head || head.height !== last || head.hash !== seen.get(last)) toHeight = await findFork(url, seen, last);
      }
    } catch (e) {
      // The first call failing is a setup problem (wrong URL, SIP-7 off):
      // say so. Later failures (node restarting) are retried.
      if (first) throw e;
      opts.onError?.(e as Error);
      await sleep(opts.intervalMs, opts.signal);
      continue;
    }
    first = false;
    if (toHeight !== null) {
      for (const h of [...seen.keys()]) if (h > toHeight) seen.delete(h);
      next = toHeight + 1;
      yield { kind: 'rollback', toHeight };
      continue;
    }
    for (const block of items) {
      seen.set(block.height, block.hash);
      seen.delete(block.height - FORK_WINDOW);
      next = block.height + 1;
      yield { kind: 'block', block };
    }
    if (items.length < MAX_RANGE - 1) await sleep(opts.intervalMs, opts.signal);
  }
}

/** Highest remembered height ≤ `top` whose hash the node still serves. */
async function findFork(url: string, seen: Map<number, string>, top: number): Promise<number> {
  const heights = [...seen.keys()].filter((h) => h <= top);
  if (heights.length === 0) return top;
  const lo = Math.min(...heights);
  const now = new Map(
    (await rpc<ZcashBlock[]>(url, 'sova_getZcashBlocks', [lo, top])).map((b) => [b.height, b.hash]),
  );
  for (let h = top; h >= lo; h--) if (now.get(h) === seen.get(h)) return h;
  // Nothing remembered survived: the fork is below all of it, and no higher
  // than what the chain anchors now. The node answers a range with its
  // anchored prefix, so the last item below `lo` is that bound. (Not the
  // precompile's anchor(): right after a Zcash reorg the Sova head is stale
  // and eth_call at latest fails until it is re-sealed.)
  const from = Math.max(0, lo - MAX_RANGE);
  const below = await rpc<ZcashBlock[]>(url, 'sova_getZcashBlocks', [from, lo - 1]);
  return below.length ? below[below.length - 1].height : from - 1;
}

/**
 * sova_subscribe("zcashBlocks", {fromHeight}) over WS (Node's built-in
 * WebSocket). The node sends rollbacks itself.
 */
export async function* wsFeed(
  url: string,
  opts: { from?: number; signal?: AbortSignal },
): AsyncGenerator<FeedEvent> {
  const ws = new WebSocket(url);
  const queue: FeedEvent[] = [];
  let wake: (() => void) | null = null;
  let failure: Error | null = null;
  let closed = false;
  const notify = () => { wake?.(); wake = null; };
  ws.addEventListener('open', () => {
    const params: unknown[] = ['zcashBlocks'];
    if (opts.from !== undefined) params.push({ fromHeight: opts.from });
    ws.send(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'sova_subscribe', params }));
  });
  ws.addEventListener('message', (m) => {
    const msg = JSON.parse(String(m.data));
    if (msg.id === 1 && msg.error) failure = new Error(`sova_subscribe: ${msg.error.message}`);
    const item = msg.method === 'sova_subscription' ? msg.params?.result : undefined;
    if (item?.rollback) queue.push({ kind: 'rollback', toHeight: item.rollback.toHeight });
    else if (item) queue.push({ kind: 'block', block: item });
    notify();
  });
  ws.addEventListener('error', () => { failure ??= new Error(`websocket error on ${url}`); notify(); });
  ws.addEventListener('close', () => { closed = true; notify(); });
  opts.signal?.addEventListener('abort', () => ws.close(), { once: true });
  try {
    for (;;) {
      while (queue.length) yield queue.shift() as FeedEvent;
      if (failure) throw failure;
      if (closed || opts.signal?.aborted) return;
      await new Promise<void>((r) => { wake = r; });
    }
  } finally {
    ws.close();
  }
}
