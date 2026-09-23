// Payment watcher: finds each open order's exact-amount payment on the
// seller's t-address through zebrad, then claims it once Sova agrees.
//
// Two views of Zcash are in play and they differ on purpose:
// - zebrad (this file) sees the Zcash tip. It is only used to FIND a
//   candidate (txid, vout): getaddresstxids on the seller's address, then
//   getrawtransaction for outputs of exactly the quote to the payee script.
// - Sova's SIP-4 precompile sees the anchored segment (lags the tip) and is
//   the only judge. A candidate is claimed when an eth_call dry run of
//   claim() succeeds; until then it waits (TxNotFound / NotYet /
//   InsufficientConfirmations just mean "anchor hasn't caught up").
// So a lying or lagging zebrad can waste a dry run, never gas or an owl.
import { CHECKOUT_ABI, describe, errorName } from './sova.mjs';
import { payeeScript, tAddr, zebrad } from './zcash.mjs';

const WAIT = new Set(['ZcashTxNotFound', 'ZcashNotYet', 'ZcashInsufficientConfirmations']);
const CHUNK = 5000n;

export function startWatcher(cfg, sova, log) {
  const z = zebrad(cfg.zcash);
  const open = new Map(); // id -> order
  const detected = new Map(); // id -> { txid, vout, height }
  const claims = new Map(); // id -> { state, txHash?, error? }
  const txCache = new Map(); // txid -> { height, outs: [{ n, value, script }] }
  let from = cfg.startBlock;
  let running = false;

  async function scanSova() {
    const latest = await sova.pub.getBlockNumber();
    while (from <= latest) {
      const to = from + CHUNK - 1n < latest ? from + CHUNK - 1n : latest;
      const q = { address: cfg.checkout, abi: CHECKOUT_ABI, fromBlock: from, toBlock: to };
      for (const l of await sova.pub.getContractEvents({ ...q, eventName: 'Reserved' })) {
        const a = l.args;
        if (cfg.listings && !cfg.listings.has(a.listingId)) continue;
        open.set(a.reservationId, {
          id: a.reservationId, quote: a.quoteZat, reservedAt: a.reservedAt, deadline: a.deadline,
          script: payeeScript(a.payeeHash, a.payeeP2sh), addr: tAddr(a.payeeHash, a.payeeP2sh, cfg.zcashNet),
        });
      }
      for (const l of await sova.pub.getContractEvents({ ...q, eventName: 'Claimed' })) {
        const id = l.args.reservationId;
        if (open.delete(id)) claims.set(id, { state: 'claimed', txHash: l.transactionHash });
      }
      from = to + 1n;
    }
  }

  async function scanZcash() {
    const tip = BigInt(await z('getblockcount'));
    const byAddr = new Map();
    for (const o of open.values()) {
      if (detected.has(o.id)) continue;
      // Not found and the window closed long ago (tip is ahead of the anchor): give up.
      if (tip > o.deadline + BigInt(cfg.dropAfter)) {
        open.delete(o.id);
        claims.set(o.id, { state: 'expired' });
        continue;
      }
      if (!byAddr.has(o.addr)) byAddr.set(o.addr, []);
      byAddr.get(o.addr).push(o);
    }
    for (const [addr, orders] of byAddr) {
      const start = orders.reduce((m, o) => (o.reservedAt < m ? o.reservedAt : m), orders[0].reservedAt) + 1n;
      if (start > tip) continue;
      const txids = await z('getaddresstxids', [{ addresses: [addr], start: Number(start), end: Number(tip) }]);
      for (const txid of txids) {
        if (!txCache.has(txid)) {
          const tx = await z('getrawtransaction', [txid, 1]);
          if (tx.height == null || tx.height < 0) continue; // mempool: look again next tick
          txCache.set(txid, {
            height: BigInt(tx.height),
            outs: (tx.vout || []).map((v) => ({ n: v.n, value: BigInt(v.valueZat), script: v.scriptPubKey?.hex })),
          });
        }
        const t = txCache.get(txid);
        for (const o of orders) {
          if (detected.has(o.id) || t.height <= o.reservedAt || t.height > o.deadline) continue;
          const out = t.outs.find((x) => x.script === o.script && x.value === o.quote);
          if (out) {
            detected.set(o.id, { txid, vout: out.n, height: t.height });
            log(`order ${o.id}: payment ${txid}:${out.n} at zcash ${t.height}`);
          }
        }
      }
    }
  }

  async function claimDetected() {
    for (const [id, d] of detected) {
      if (!open.has(id)) continue;
      const c = claims.get(id);
      if (c && c.state !== 'waiting') continue;
      const txid = '0x' + d.txid;
      try {
        await sova.simulateClaim(id, txid, d.vout);
      } catch (e) {
        const name = errorName(e);
        if (name === 'AlreadyFilled') {
          open.delete(id);
          claims.set(id, { state: 'claimed' });
        } else if (WAIT.has(name)) {
          claims.set(id, { state: 'waiting', error: describe(e) });
        } else {
          claims.set(id, { state: 'failed', error: describe(e) });
          log(`order ${id}: not claimable: ${describe(e)}`);
        }
        continue;
      }
      claims.set(id, { state: 'sending' });
      try {
        const { hash } = await sova.send('claim', [id, txid, d.vout]);
        open.delete(id);
        claims.set(id, { state: 'claimed', txHash: hash });
        log(`order ${id}: claimed in ${hash}`);
      } catch (e) {
        claims.set(id, { state: 'waiting', error: describe(e) });
      }
    }
  }

  async function tick() {
    if (running) return;
    running = true;
    try {
      await scanSova();
      await scanZcash();
      await claimDetected();
    } catch (e) {
      log(`watcher: ${describe(e)}`);
    } finally {
      running = false;
    }
  }

  const timer = setInterval(tick, cfg.pollMs);
  tick();
  return {
    stop: () => clearInterval(timer),
    status(id) {
      const d = detected.get(id);
      return {
        watching: open.has(id),
        detected: d ? { txid: d.txid, vout: d.vout, height: Number(d.height) } : null,
        claim: claims.get(id) ?? null,
      };
    },
  };
}
