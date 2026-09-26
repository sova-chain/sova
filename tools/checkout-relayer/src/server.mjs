#!/usr/bin/env node
// Ashwings ZEC checkout relayer (docs/design/ashwing-zec-checkout.md).
//
//   POST /reserve {listingId, recipient}       reserve for a buyer; relayer pays gas
//   POST /claim   {reservationId, txid, vout}  claim a paid order (dry-run first)
//   GET  /status                               relayer summary (no secrets)
//   GET  /status/:id                           what the watcher found for an order
//   GET  /health                               liveness (local; not routed publicly)
//
// POST answers once the transaction is mined if that takes under
// RESPOND_WAIT_MS (200 with the result), else as soon as it is broadcast
// (202 {txHash, pending: true}; the page then waits for the receipt itself):
// a Sova block can take longer than a proxy will hold a request open.
//
// It can't steal anything: reserve and claim always deliver to the order's
// recipient; the relayer only spends gas. What it guards on the public
// internet (README.md, "Running it publicly"): its SOVA (per-IP and global
// rate limits, a balance floor below which it takes no new orders), the owl
// supply (at most MAX_OPEN_RESERVATIONS of its own unpaid orders at once,
// one per recipient), and its RPC budget (one throttle for every Sova call).
// Config is env only (see README.md); nothing secret is committed, logged
// or served.
import { readFileSync, renameSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { formatEther } from 'viem';
import { clientKey, windows } from './limits.mjs';
import { connectSova, describe, errorName, outOfFunds } from './sova.mjs';
import { startWatcher } from './watcher.mjs';

function env(name, fallback) {
  const v = process.env[name];
  if (v === undefined || v === '') {
    if (fallback === undefined) throw new Error(`missing env ${name}`);
    return fallback;
  }
  return v;
}
const int = (name, fallback) => {
  const v = Number(env(name, String(fallback)));
  if (!Number.isSafeInteger(v) || v < 0) throw new Error(`env ${name} must be a non-negative integer`);
  return v;
};
const addrOrEmpty = (name) => {
  const v = env(name, '');
  if (v && !/^0x[0-9a-fA-F]{40}$/.test(v)) throw new Error(`env ${name} is not an address`);
  return v;
};

// TRUST_PROXY=1 is the older spelling of TRUST_PROXY_HEADER=x-forwarded-for.
const proxyHeader = env('TRUST_PROXY_HEADER', env('TRUST_PROXY', '0') === '1' ? 'x-forwarded-for' : '').toLowerCase();
if (proxyHeader && !['cf-connecting-ip', 'x-forwarded-for', 'x-real-ip'].includes(proxyHeader)) {
  throw new Error('TRUST_PROXY_HEADER must be cf-connecting-ip, x-forwarded-for or x-real-ip');
}

const cfg = {
  sovaRpc: env('SOVA_RPC_URL', 'http://127.0.0.1:8545'),
  checkout: addrOrEmpty('CHECKOUT'),
  ashwings: addrOrEmpty('ASHWINGS'),
  relayerKey: env('RELAYER_KEY'),
  listings: process.env.LISTINGS ? new Set(process.env.LISTINGS.split(',').map((s) => BigInt(s.trim()))) : null,
  host: env('HOST', '127.0.0.1'),
  port: int('PORT', 8787),
  corsOrigins: env('CORS_ORIGIN', '*').split(',').map((s) => s.trim()).filter(Boolean),
  proxyHeader,
  maxBody: int('MAX_BODY_BYTES', 1024),
  reservePerIpHour: int('RESERVE_PER_IP_PER_HOUR', 3),
  reservePerHour: int('RESERVE_PER_HOUR', 60),
  reservePerMinute: int('RESERVE_PER_MINUTE', 10),
  claimPerIpHour: int('CLAIM_PER_IP_PER_HOUR', 20),
  claimPerMinute: int('CLAIM_PER_MINUTE', 30),
  maxOpen: int('MAX_OPEN_RESERVATIONS', 20),
  reuseMinBlocks: int('REUSE_MIN_BLOCKS_LEFT', 20),
  minBalanceWei: BigInt(env('MIN_BALANCE_WEI', '100000000000000000')), // 0.1 SOVA
  respondWaitMs: int('RESPOND_WAIT_MS', 25_000),
  stateFile: env('STATE_FILE', ''),
  pruneMs: int('PRUNE_MS', 30_000),
  rpcPer10s: int('RPC_PER_10S', 30),
  rpcPollMs: int('RPC_POLL_MS', 4000),
  logChunk: int('LOG_CHUNK', 1000),
  zcash: process.env.ZCASH_RPC_URL
    ? {
        url: process.env.ZCASH_RPC_URL,
        user: process.env.ZCASH_RPC_USER,
        password: process.env.ZCASH_RPC_PASSWORD,
        cookieFile: process.env.ZCASH_RPC_COOKIE,
      }
    : null,
  zcashNet: env('ZCASH_NET', 'test'),
  pollMs: int('POLL_MS', 5000),
  startBlock: BigInt(env('START_BLOCK', '0')),
  dropAfter: int('DROP_AFTER_BLOCKS', 20),
};
// The key only ever lives in cfg.relayerKey; keep it out of the environment
// that child processes, crash dumps or a stray `env` would see.
delete process.env.RELAYER_KEY;

const log = (m) => console.log(`${new Date().toISOString()} ${m}`);
const sova = await connectSova(cfg);
const W = windows();
const HOUR = 3_600_000;
const MIN = 60_000;
const isAddr = (a) => typeof a === 'string' && /^0x[0-9a-fA-F]{40}$/.test(a) && !/^0x0{40}$/.test(a);
const isUint = (v) => (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0) || (typeof v === 'string' && /^\d{1,30}$/.test(v));
const json = (body) => JSON.stringify(body, (_, v) => (typeof v === 'bigint' ? v.toString() : v));

// ---- the relayer's own open orders ---------------------------------------
// Each reservation this relayer made that can still be paid: pending (sent,
// not mined yet) or mined, unfilled and at most `deadline` (the last Zcash
// height a payment may be mined at). They count against
// MAX_OPEN_RESERVATIONS, so however many IPs a spammer uses, the relayer
// holds at most that many owls for unpaid orders at a time (and each hold
// lapses on its own: AshwingsZecCheckout.holdBlocks). Kept in STATE_FILE so
// a restart doesn't reset the count.
//   key: tx hash; value: { txHash, recipient, id?, deadline?, quoteZat?, at }
const orders = new Map();
let anchor = null; // last Zcash anchor seen through Sova

function loadState() {
  if (!cfg.stateFile) return;
  try {
    const s = JSON.parse(readFileSync(cfg.stateFile, 'utf8'));
    for (const o of s.orders || []) {
      orders.set(o.txHash, {
        ...o,
        id: o.id != null ? BigInt(o.id) : undefined,
        deadline: o.deadline != null ? BigInt(o.deadline) : undefined,
        quoteZat: o.quoteZat != null ? BigInt(o.quoteZat) : undefined,
      });
    }
    log(`state: ${orders.size} open order(s) from ${cfg.stateFile}`);
  } catch (e) {
    if (e.code !== 'ENOENT') log(`state: ${cfg.stateFile} unreadable (${e.message}); starting empty`);
  }
}
function saveState() {
  if (!cfg.stateFile) return;
  const tmp = `${cfg.stateFile}.tmp`;
  writeFileSync(tmp, json({ orders: [...orders.values()] }), { mode: 0o600 });
  renameSync(tmp, cfg.stateFile);
}

/** Records a mined reservation from its receipt logs; drops the order if it failed. */
function settle(txHash, mined) {
  return mined.then(
    ({ logs }) => {
      const ev = logs.find((l) => l.eventName === 'Reserved');
      const o = orders.get(txHash);
      if (!ev) {
        if (orders.delete(txHash)) saveState();
        throw new Error(`no Reserved event in ${txHash}`);
      }
      if (o) {
        Object.assign(o, { id: ev.args.reservationId, deadline: ev.args.deadline, quoteZat: ev.args.quoteZat });
        saveState();
      }
      log(`reserve: order ${ev?.args.reservationId} for ${o?.recipient} in ${txHash}`);
      return ev;
    },
    (e) => {
      if (orders.delete(txHash)) saveState();
      log(`reserve ${txHash} failed: ${describe(e)}`);
      throw e;
    },
  );
}

let pruning = false;
async function prune() {
  // Never two at once: a slow RPC must not stack up passes in the throttle.
  if (pruning) return;
  pruning = true;
  try {
    anchor = await sova.anchor();
    let changed = false;
    for (const [k, o] of orders) {
      if (o.id === undefined) {
        // Pending across a restart: look for its receipt.
        if (Date.now() - o.at > 15 * MIN) {
          const r = await sova.receipt(o.txHash);
          if (r) await settle(k, Promise.resolve(r)).catch(() => {});
          else if (orders.delete(k)) changed = true;
        }
        continue;
      }
      if (anchor > o.deadline) {
        orders.delete(k);
        changed = true;
      } else if (!watcher && (await sova.reservation(o.id)).filled) {
        orders.delete(k);
        changed = true;
      }
    }
    if (changed) saveState();
  } catch (e) {
    log(`prune: ${describe(e)}`);
  } finally {
    pruning = false;
  }
}
function dropFilled(id) {
  for (const [k, o] of orders) {
    if (o.id === id) {
      orders.delete(k);
      saveState();
    }
  }
}
// ---- balance (cached: /status is public) -----------------------------------
let bal = { wei: null, at: 0 };
async function balance(maxAgeMs = 15_000) {
  if (bal.wei === null || Date.now() - bal.at > maxAgeMs) bal = { wei: await sova.balance(), at: Date.now() };
  return bal.wei;
}

// ---- one in-flight claim per order (POST /claim and the watcher share it) ----
const claiming = new Map(); // id -> Promise<{ hash, done }>
function claimOnce(id, txid, vout) {
  if (claiming.has(id)) return claiming.get(id);
  const entry = sova.broadcast('claim', [id, txid, vout]).then(({ hash, mined }) => {
    bal.at = 0;
    const done = mined.then(({ logs }) => {
      const ev = logs.find((l) => l.eventName === 'Claimed');
      log(`claim: order ${id} -> item ${ev?.args.itemId} in ${hash}`);
      dropFilled(id);
      return { hash, itemId: ev?.args.itemId };
    });
    // Mined: keep answering with this claim for a while (a double click
    // gets the same tx). Failed: forget it, so a retry can go out.
    done.then(
      () => setTimeout(() => claiming.delete(id), 10 * MIN).unref(),
      () => claiming.delete(id),
    );
    return { hash, done };
  });
  entry.catch(() => claiming.delete(id)); // the dry run said no: nothing was sent
  claiming.set(id, entry);
  return entry;
}

// ---- start ----------------------------------------------------------------------
// The watcher claims through claimOnce, so it starts once that exists.
const watcher = cfg.zcash
  ? startWatcher(cfg, sova, log, { claim: (id, txid, vout) => claimOnce(id, txid, vout).then((c) => c.done) })
  : null;
watcher?.onClaimed(dropFilled);
loadState();
await prune();
setInterval(prune, cfg.pruneMs).unref();

/** Resolves `p` if it settles within `ms`, else null. */
const within = (p, ms) => Promise.race([p, new Promise((r) => setTimeout(() => r(null), ms).unref())]);

// ---- HTTP ----------------------------------------------------------------------
class HttpError extends Error {
  constructor(code, message, headers = {}) {
    super(message);
    this.code = code;
    this.headers = headers;
  }
}

function corsFor(req) {
  const origin = req.headers.origin;
  if (cfg.corsOrigins.includes('*')) return '*';
  return origin && cfg.corsOrigins.includes(origin) ? origin : null;
}

function send(req, res, code, body, headers = {}) {
  const allow = corsFor(req);
  res.writeHead(code, {
    'content-type': 'application/json',
    'cache-control': 'no-store',
    'x-content-type-options': 'nosniff',
    vary: 'origin',
    ...(allow
      ? {
          'access-control-allow-origin': allow,
          'access-control-allow-methods': 'GET, POST, OPTIONS',
          'access-control-allow-headers': 'content-type',
          'access-control-expose-headers': 'retry-after',
          'access-control-max-age': '7200',
        }
      : {}),
    ...headers,
  });
  res.end(code === 204 ? undefined : json(body));
}

async function readJson(req) {
  // A browser from another site gets no CORS grant, but a "simple" POST
  // (text/plain, no preflight) would still arrive: refuse both.
  if (req.headers.origin && !corsFor(req)) throw new HttpError(403, 'origin not allowed');
  if (!/^application\/json\b/i.test(req.headers['content-type'] || '')) throw new HttpError(415, 'send application/json');
  const declared = Number(req.headers['content-length'] || 0);
  if (declared > cfg.maxBody) throw new HttpError(413, 'body too large');
  let raw = '';
  for await (const chunk of req) {
    raw += chunk;
    if (raw.length > cfg.maxBody) throw new HttpError(413, 'body too large');
  }
  try {
    const v = JSON.parse(raw || '{}');
    if (typeof v !== 'object' || v === null || Array.isArray(v)) throw new Error();
    return v;
  } catch {
    throw new HttpError(400, 'bad JSON');
  }
}

function limit(key, n, ms, message) {
  if (!W.allow(key, n, ms)) throw new HttpError(429, message, { 'retry-after': String(W.retryAfter(key)) });
}

const openCount = () => orders.size;

async function status() {
  const wei = await balance();
  const accepting = wei >= cfg.minBalanceWei && openCount() < cfg.maxOpen;
  return {
    ok: true,
    relayer: sova.address,
    chainId: sova.chainId,
    checkout: sova.checkout,
    listings: cfg.listings ? [...cfg.listings] : 'any',
    balanceWei: wei,
    balanceSova: formatEther(wei),
    minBalanceWei: cfg.minBalanceWei,
    accepting,
    reason: accepting ? null : wei < cfg.minBalanceWei ? 'low balance' : 'busy: too many open orders',
    openReservations: openCount(),
    maxOpenReservations: cfg.maxOpen,
    zcashAnchor: anchor,
    watching: Boolean(watcher),
    limits: {
      reservePerIpPerHour: cfg.reservePerIpHour,
      reservePerHour: cfg.reservePerHour,
      reservePerMinute: cfg.reservePerMinute,
      claimPerIpPerHour: cfg.claimPerIpHour,
    },
  };
}

async function reserve(req) {
  const b = await readJson(req);
  if (!isAddr(b.recipient) || !isUint(b.listingId)) throw new HttpError(400, 'need {listingId, recipient}');
  const listingId = BigInt(b.listingId);
  if (cfg.listings && !cfg.listings.has(listingId)) throw new HttpError(403, 'listing not served here');
  const recipient = b.recipient.toLowerCase();

  // A recipient with an open order from us gets that order back (a reload
  // or a double click costs nothing), while it has time left to pay.
  for (const o of orders.values()) {
    if (o.recipient !== recipient) continue;
    if (o.id === undefined) return [202, { txHash: o.txHash, pending: true, existing: true }];
    if (anchor !== null && o.deadline - anchor >= BigInt(cfg.reuseMinBlocks)) {
      return [200, { reservationId: o.id, quoteZat: o.quoteZat, txHash: o.txHash, existing: true }];
    }
  }

  const ip = clientKey(req, cfg.proxyHeader);
  limit(`r:${ip}`, cfg.reservePerIpHour, HOUR, `too many reservations from your address: try again later, or reserve with a wallet`);
  limit('r:*:m', cfg.reservePerMinute, MIN, 'the relayer is busy: try again in a minute');
  limit('r:*:h', cfg.reservePerHour, HOUR, 'the relayer has reached its hourly limit: try later, or reserve with a wallet');
  if (openCount() >= cfg.maxOpen) {
    throw new HttpError(503, `the relayer has ${openCount()} unpaid orders open (its limit): try again in a while, or reserve with a wallet`, { 'retry-after': '600' });
  }
  if ((await balance()) < cfg.minBalanceWei) {
    throw new HttpError(503, 'the relayer is low on SOVA for gas and takes no new orders: reserve with a wallet, or try later', { 'retry-after': '3600' });
  }

  const { hash, mined } = await sova.broadcast('reserve', [listingId, b.recipient]);
  bal.at = 0;
  orders.set(hash, { txHash: hash, recipient, at: Date.now() });
  saveState();
  const ev = await within(settle(hash, mined), cfg.respondWaitMs);
  if (!ev) return [202, { txHash: hash, pending: true }];
  return [200, { reservationId: ev.args.reservationId, quoteZat: ev.args.quoteZat, txHash: hash }];
}

async function claim(req) {
  const b = await readJson(req);
  const txid = String(b.txid || '').replace(/^0x/, '').toLowerCase();
  if (!isUint(b.reservationId) || !/^[0-9a-f]{64}$/.test(txid) || !isUint(b.vout) || Number(b.vout) > 0xffffffff) {
    throw new HttpError(400, 'need {reservationId, txid, vout}');
  }
  const ip = clientKey(req, cfg.proxyHeader);
  limit(`c:${ip}`, cfg.claimPerIpHour, HOUR, 'too many claims from your address: try again later');
  limit('c:*:m', cfg.claimPerMinute, MIN, 'the relayer is busy: try again in a minute');
  // No balance floor here: a claim is for an order someone already paid ZEC
  // for, and the floor exists to leave gas for exactly these.
  const { hash, done } = await claimOnce(BigInt(b.reservationId), `0x${txid}`, Number(b.vout));
  const r = await within(done, cfg.respondWaitMs);
  if (!r) return [202, { txHash: hash, pending: true }];
  return [200, { itemId: r.itemId, txHash: hash }];
}

async function handle(req, res) {
  const url = new URL(req.url, 'http://x');
  if (req.method === 'OPTIONS') return send(req, res, 204, null);

  if (req.method === 'GET' && url.pathname === '/health') {
    return send(req, res, 200, { ok: true, relayer: sova.address, watching: Boolean(watcher) });
  }
  if (req.method === 'GET' && url.pathname === '/status') return send(req, res, 200, await status());

  const m = url.pathname.match(/^\/status\/(\d{1,30})$/);
  if (req.method === 'GET' && m) {
    return send(req, res, 200, watcher ? watcher.status(BigInt(m[1])) : { watching: false, detected: null, claim: null });
  }

  if (req.method === 'POST' && url.pathname === '/reserve') return send(req, res, ...(await reserve(req)));
  if (req.method === 'POST' && url.pathname === '/claim') return send(req, res, ...(await claim(req)));

  send(req, res, 404, { error: 'not found' });
}

function fail(req, res, e) {
  if (res.headersSent) return res.destroy();
  if (e instanceof HttpError) return send(req, res, e.code, { error: e.message }, e.headers);
  if (outOfFunds(e)) {
    bal.at = 0;
    return send(req, res, 503, { error: 'the relayer is out of SOVA for gas: use a wallet, or try later' });
  }
  const name = errorName(e);
  if (name) return send(req, res, 400, { error: describe(e) }); // the contract said no
  log(`error: ${describe(e)}`);
  send(req, res, 502, { error: 'the relayer could not reach Sova: try again shortly' });
}

const server = http.createServer((req, res) => handle(req, res).catch((e) => fail(req, res, e)));
// Slow or oversized clients: the header and body must arrive promptly.
server.headersTimeout = 10_000;
server.requestTimeout = 15_000;
server.keepAliveTimeout = 5_000;
server.maxHeadersCount = 50;
server.listen(cfg.port, cfg.host, () => {
  log(
    `checkout relayer ${sova.address} on http://${cfg.host}:${cfg.port} · chain ${sova.chainId} · checkout ${sova.checkout}` +
      ` · listings ${cfg.listings ? [...cfg.listings].join(',') : 'any'} · cors ${cfg.corsOrigins.join(',')}` +
      ` · client ip ${cfg.proxyHeader || 'socket'} · max open ${cfg.maxOpen} · floor ${formatEther(cfg.minBalanceWei)} SOVA` +
      ` · watcher ${watcher ? 'on' : 'off'}`,
  );
});
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => { watcher?.stop(); server.close(); process.exit(0); });
