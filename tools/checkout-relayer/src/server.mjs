#!/usr/bin/env node
// Ashwings ZEC checkout relayer (docs/design/ashwing-zec-checkout.md).
//
//   POST /reserve {listingId, recipient}   reserve for a buyer; relayer pays gas
//   POST /claim   {reservationId, txid, vout}  claim a paid order (dry-run first)
//   GET  /status/:id                       what the watcher found for an order
//   GET  /health
//
// It can't steal anything: reserve and claim always deliver to the order's
// recipient; the relayer only spends gas. Rate limits bound that spend.
// Config is env only (see README.md); nothing secret is committed.
import http from 'node:http';
import { connectSova, describe } from './sova.mjs';
import { startWatcher } from './watcher.mjs';

function env(name, fallback) {
  const v = process.env[name];
  if (v === undefined || v === '') {
    if (fallback === undefined) throw new Error(`missing env ${name}`);
    return fallback;
  }
  return v;
}

const cfg = {
  sovaRpc: env('SOVA_RPC_URL', 'http://127.0.0.1:8545'),
  checkout: env('CHECKOUT'),
  relayerKey: env('RELAYER_KEY'),
  listings: process.env.LISTINGS ? new Set(process.env.LISTINGS.split(',').map((s) => BigInt(s.trim()))) : null,
  host: env('HOST', '127.0.0.1'),
  port: Number(env('PORT', '8787')),
  corsOrigin: env('CORS_ORIGIN', '*'),
  trustProxy: env('TRUST_PROXY', '0') === '1',
  reservePerIpHour: Number(env('RESERVE_PER_IP_PER_HOUR', '10')),
  claimPerIpHour: Number(env('CLAIM_PER_IP_PER_HOUR', '30')),
  reservePerMinute: Number(env('RESERVE_PER_MINUTE', '30')),
  zcash: process.env.ZCASH_RPC_URL
    ? {
        url: process.env.ZCASH_RPC_URL,
        user: process.env.ZCASH_RPC_USER,
        password: process.env.ZCASH_RPC_PASSWORD,
        cookieFile: process.env.ZCASH_RPC_COOKIE,
      }
    : null,
  zcashNet: env('ZCASH_NET', 'test'),
  pollMs: Number(env('POLL_MS', '5000')),
  startBlock: BigInt(env('START_BLOCK', '0')),
  dropAfter: Number(env('DROP_AFTER_BLOCKS', '20')),
};

const log = (m) => console.log(`${new Date().toISOString()} ${m}`);
const sova = await connectSova(cfg);
const watcher = cfg.zcash ? startWatcher(cfg, sova, log) : null;

// Fixed-window counters: key -> { n, reset }.
const windows = new Map();
function allow(key, limit, ms) {
  const now = Date.now();
  const w = windows.get(key);
  if (!w || now >= w.reset) {
    windows.set(key, { n: 1, reset: now + ms });
    return true;
  }
  return ++w.n <= limit;
}
setInterval(() => {
  const now = Date.now();
  for (const [k, w] of windows) if (now >= w.reset) windows.delete(k);
}, 60_000).unref();

const HOUR = 3_600_000;
const isAddr = (a) => typeof a === 'string' && /^0x[0-9a-fA-F]{40}$/.test(a) && !/^0x0{40}$/.test(a);
const isUint = (v) => (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0) || (typeof v === 'string' && /^\d{1,30}$/.test(v));

function send(res, code, body) {
  res.writeHead(code, {
    'content-type': 'application/json',
    'access-control-allow-origin': cfg.corsOrigin,
    'access-control-allow-methods': 'GET, POST, OPTIONS',
    'access-control-allow-headers': 'content-type',
  });
  res.end(JSON.stringify(body, (_, v) => (typeof v === 'bigint' ? v.toString() : v)));
}

async function readJson(req) {
  let raw = '';
  for await (const chunk of req) {
    raw += chunk;
    if (raw.length > 4096) throw new Error('body too large');
  }
  return JSON.parse(raw || '{}');
}

const ipOf = (req) =>
  (cfg.trustProxy && String(req.headers['x-forwarded-for'] || '').split(',')[0].trim()) || req.socket.remoteAddress;

async function handle(req, res) {
  const url = new URL(req.url, 'http://x');
  if (req.method === 'OPTIONS') return send(res, 204, {});

  if (req.method === 'GET' && url.pathname === '/health') {
    const balance = await sova.pub.getBalance({ address: sova.address });
    return send(res, 200, {
      ok: true, chainId: sova.chainId, checkout: cfg.checkout, relayer: sova.address, balanceWei: balance,
      watching: Boolean(watcher),
    });
  }

  const m = url.pathname.match(/^\/status\/(\d{1,30})$/);
  if (req.method === 'GET' && m) {
    return send(res, 200, watcher ? watcher.status(BigInt(m[1])) : { watching: false, detected: null, claim: null });
  }

  if (req.method === 'POST' && url.pathname === '/reserve') {
    const b = await readJson(req);
    if (!isAddr(b.recipient) || !isUint(b.listingId)) return send(res, 400, { error: 'need {listingId, recipient}' });
    const listingId = BigInt(b.listingId);
    if (cfg.listings && !cfg.listings.has(listingId)) return send(res, 403, { error: 'listing not served here' });
    if (!allow(`r:${ipOf(req)}`, cfg.reservePerIpHour, HOUR) || !allow('r:*', cfg.reservePerMinute, 60_000)) {
      return send(res, 429, { error: 'too many reservations: try later or use a wallet' });
    }
    const { hash, logs } = await sova.send('reserve', [listingId, b.recipient]);
    const ev = logs.find((l) => l.eventName === 'Reserved');
    log(`reserve: order ${ev.args.reservationId} for ${b.recipient} in ${hash}`);
    return send(res, 200, { reservationId: ev.args.reservationId, quoteZat: ev.args.quoteZat, txHash: hash });
  }

  if (req.method === 'POST' && url.pathname === '/claim') {
    const b = await readJson(req);
    const txid = String(b.txid || '').replace(/^0x/, '').toLowerCase();
    if (!isUint(b.reservationId) || !/^[0-9a-f]{64}$/.test(txid) || !isUint(b.vout)) {
      return send(res, 400, { error: 'need {reservationId, txid, vout}' });
    }
    if (!allow(`c:${ipOf(req)}`, cfg.claimPerIpHour, HOUR)) return send(res, 429, { error: 'too many claims' });
    // send() dry-runs first, so a claim that would revert costs no gas.
    const { hash, logs } = await sova.send('claim', [BigInt(b.reservationId), `0x${txid}`, Number(b.vout)]);
    const ev = logs.find((l) => l.eventName === 'Claimed');
    log(`claim: order ${b.reservationId} -> item ${ev.args.itemId} in ${hash}`);
    return send(res, 200, { itemId: ev.args.itemId, txHash: hash });
  }

  send(res, 404, { error: 'not found' });
}

const server = http.createServer((req, res) =>
  handle(req, res).catch((e) => send(res, 400, { error: describe(e) })),
);
server.listen(cfg.port, cfg.host, () => {
  log(`checkout relayer ${sova.address} on http://${cfg.host}:${cfg.port} · chain ${sova.chainId} · checkout ${cfg.checkout} · watcher ${watcher ? 'on' : 'off'}`);
});
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => { watcher?.stop(); server.close(); process.exit(0); });
