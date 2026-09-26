// infra/testnet/worker/rpc-firewall.mjs -- the edge half of "read-and-
// broadcast only, enforced twice" (docs/design/infra-m1.md §2). Runs as a
// Cloudflare Worker on the route rpc.<domain>/*; fetch(request) then goes
// to the origin (the rpc-1 tunnel), whose SOVA_RPC_PROFILE=public enforces
// the same method list again. Rate-limit rules can't read bodies, so the
// method allowlist, batch cap and size cap live here. So does the per-IP
// rate limit (a Workers Rate Limiting binding, below), so its 429 carries
// CORS headers a browser page can read.
//
// Keep ALLOWED in sync with PUBLIC_RPC_METHODS in bin/sova/src/rpc.rs
// (worker/test.mjs checks that).

export const ALLOWED = new Set([
  // Chain metadata and fees.
  "eth_chainId",
  "net_version",
  "web3_clientVersion",
  "eth_syncing",
  "eth_blockNumber",
  "eth_gasPrice",
  "eth_maxPriorityFeePerGas",
  "eth_feeHistory",
  // Blocks and transactions.
  "eth_getBlockByNumber",
  "eth_getBlockByHash",
  "eth_getBlockReceipts",
  "eth_getTransactionByHash",
  "eth_getTransactionByBlockHashAndIndex",
  "eth_getTransactionByBlockNumberAndIndex",
  "eth_getTransactionReceipt",
  "eth_getTransactionCount",
  // State and calls.
  "eth_getBalance",
  "eth_getCode",
  "eth_getStorageAt",
  "eth_call",
  "eth_estimateGas",
  "eth_getLogs",
  // Broadcast.
  "eth_sendRawTransaction",
  // Broadcast and wait for the receipt (EIP-7966); its timeout is capped
  // at SYNC_TIMEOUT_MS below.
  "eth_sendRawTransactionSync",
  // SIP-7 Zcash block feed (read-only; the node caps it at 1,000 heights
  // per call and serves it only with SOVA_SIP7=1).
  "sova_getZcashBlocks",
]);

export const MAX_BODY_BYTES = 64 * 1024;
export const MAX_BATCH = 10;
export const MAX_LOG_RANGE = 1000;
// eth_sendRawTransactionSync holds the request open until the
// transaction's block, and a Sova block follows each Zcash block (~75 s,
// often a few minutes). Cloudflare gives up on an origin that hasn't
// answered in ~100 s (HTTP 524, no JSON-RPC body), so the edge caps the
// call's own timeout (its optional second param, timeout_ms) at 90 s. A
// slow block then comes back as the node's JSON-RPC timeout error, with
// the transaction hash, before the 524. The node's cap
// (SOVA_SEND_SYNC_TIMEOUT_SECS, 300 s by default) is longer; reth takes
// the smaller. So through this endpoint the effective limit is 90 s, and a
// client that gets the timeout error polls eth_getTransactionReceipt.
export const SYNC_METHOD = "eth_sendRawTransactionSync";
export const SYNC_TIMEOUT_MS = 90_000;

// A sync call with timeout_ms capped at SYNC_TIMEOUT_MS (added when
// absent, null or 0, which reth reads as "the node's own cap"); any other
// call unchanged. A non-numeric timeout is left for the origin to refuse.
export function capSync(call) {
  if (call.method !== SYNC_METHOD) return call;
  const cap = (v) => (v === undefined || v === null || v === 0 || (typeof v === "number" && v > SYNC_TIMEOUT_MS) ? SYNC_TIMEOUT_MS : v);
  const p = call.params;
  if (Array.isArray(p)) {
    const out = [...p];
    if (out.length === 0) return call; // no transaction: the origin refuses it
    out[1] = cap(out[1]);
    return { ...call, params: out };
  }
  if (p && typeof p === "object") {
    const out = { ...p };
    if ("timeoutMs" in out) out.timeoutMs = cap(out.timeoutMs);
    else out.timeout_ms = cap(out.timeout_ms);
    return { ...call, params: out };
  }
  return call;
}

// 82330 = the sova-testnet chain ID (bin/sova/src/chain.rs). Answered here
// without touching the origin.
const CHAIN_ID_HEX = "0x1419a";
const CHAIN_ID_DEC = "82330";

// CORS for browser JSON-RPC (sova.io's /pulse and /ashwings pages call
// this endpoint with fetch). "*" is deliberate: the endpoint is public and
// read-and-broadcast only, and it carries no cookies or credentials, so no
// origin needs to be singled out. Every response the Worker produces --
// preflight, success, refusal, rate limit, error, origin failure --
// carries these. Retry-After is not a CORS-safelisted response header, so
// it is exposed explicitly: a page reads it from the 429 and backs off.
export const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "POST, OPTIONS",
  "access-control-allow-headers": "content-type",
  "access-control-expose-headers": "retry-after",
  // 24 h; Chromium caps it at 2 h. Preflights are never rate-limited, but
  // fewer of them is still less latency for the page.
  "access-control-max-age": "86400",
};

// Per-IP rate limit. cloudflare.sh uploads the Worker with a Workers Rate
// Limiting binding named RPC_RATELIMIT ({limit, period} from config.env:
// RPC_RATELIMIT_REQUESTS per RPC_RATELIMIT_PERIOD s, default 50 per 10 s,
// the numbers the zone's WAF rule used when it limited "/") and plain-text
// vars RPC_RATELIMIT_REQUESTS / RPC_RATELIMIT_PERIOD for the error text and
// Retry-After. The key is the client IP (CF-Connecting-IP, set by
// Cloudflare's edge; a client can't forge it through the proxy). OPTIONS
// preflights are not counted. Counters are per Cloudflare location and
// eventually consistent, like the WAF rule's (per colo + IP).
//
// No binding (a hand upload, a dashboard edit, RPC_RATELIMIT_AT=waf):
// fail OPEN, with a warning logged once per isolate. The method allowlist,
// batch/size caps and the origin's own public profile still apply, and a
// fail-closed Worker would turn a deploy slip into a public-RPC outage.
// With RPC_RATELIMIT_AT=waf the zone's WAF rule covers "/" again (see
// cloudflare.sh), so the missing binding is expected and not logged.
export const RATELIMIT_DEFAULT_PERIOD_S = 10;
let warnedNoLimiter = false;

function positiveInt(v, fallback) {
  const n = Number(v);
  return Number.isInteger(n) && n > 0 ? n : fallback;
}

// A 429 Response if this request is over the limit, else null.
async function rateLimit(request, env) {
  const limiter = env?.RPC_RATELIMIT;
  if (!limiter || typeof limiter.limit !== "function") {
    if (!warnedNoLimiter && env?.RPC_RATELIMIT_AT !== "waf") {
      warnedNoLimiter = true;
      console.warn("rpc-firewall: no RPC_RATELIMIT binding; per-IP rate limit is OFF (fail open). Re-run cloudflare.sh worker.");
    }
    return null;
  }
  const key = request.headers.get("cf-connecting-ip") || "unknown";
  let outcome;
  try {
    outcome = await limiter.limit({ key });
  } catch (e) {
    console.warn(`rpc-firewall: rate limiter error, failing open: ${e}`);
    return null;
  }
  if (outcome?.success !== false) return null;
  const period = positiveInt(env.RPC_RATELIMIT_PERIOD, RATELIMIT_DEFAULT_PERIOD_S);
  const limit = positiveInt(env.RPC_RATELIMIT_REQUESTS, null);
  const what = limit ? `${limit} requests per ${period} s per IP` : `the per-IP request rate`;
  // -32005 "limit exceeded" (EIP-1474). id is null: the body is not read.
  return json(rpcError(null, -32005, `rate limited: over ${what}; retry after ${period} s`), 429, {
    "retry-after": String(period),
  });
}

function json(body, status = 200, extra = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...CORS, ...extra },
  });
}

function rpcError(id, code, message) {
  return { jsonrpc: "2.0", id: id ?? null, error: { code, message } };
}

function hexNum(v) {
  return typeof v === "string" && /^0x[0-9a-fA-F]{1,16}$/.test(v) ? Number.parseInt(v, 16) : null;
}

// Returns an error object for a call the edge refuses, else null.
export function check(call) {
  if (call === null || typeof call !== "object" || Array.isArray(call)) {
    return rpcError(null, -32600, "invalid request");
  }
  if (typeof call.method !== "string" || !ALLOWED.has(call.method)) {
    return rpcError(call.id, -32601, `method not available on the public endpoint: ${String(call.method)}`);
  }
  if (call.method === "eth_getLogs") {
    const f = Array.isArray(call.params) ? call.params[0] : null;
    if (f && typeof f === "object" && f.blockHash === undefined) {
      const from = hexNum(f.fromBlock);
      const to = hexNum(f.toBlock);
      // Named tags ("latest", ...) are left to the origin's own cap.
      if (from !== null && to !== null && to - from > MAX_LOG_RANGE) {
        return rpcError(call.id, -32005, `eth_getLogs range is capped at ${MAX_LOG_RANGE} blocks`);
      }
    }
  }
  return null;
}

// Local answer for calls that never need the origin, else undefined.
function local(call) {
  if (call.method === "eth_chainId") return { jsonrpc: "2.0", id: call.id ?? null, result: CHAIN_ID_HEX };
  if (call.method === "net_version") return { jsonrpc: "2.0", id: call.id ?? null, result: CHAIN_ID_DEC };
  return undefined;
}

export default {
  async fetch(request, env) {
    try {
      return await handle(request, env);
    } catch {
      // Without this an exception becomes Cloudflare's 1101 page, which
      // has no CORS headers.
      return json(rpcError(null, -32603, "internal error at the edge"), 500);
    }
  },
};

async function handle(request, env) {
  // Preflights are answered before the limiter: they don't count.
  if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
  // Everything else counts, before the body is read (as the WAF rule did).
  const limited = await rateLimit(request, env);
  if (limited) return limited;
  if (request.method !== "POST") {
    return json(rpcError(null, -32600, "POST JSON-RPC to this URL (Sova testnet, chain ID 82330)"), 405, {
      allow: "POST, OPTIONS",
    });
  }
  const declared = Number(request.headers.get("content-length") ?? "0");
  if (declared > MAX_BODY_BYTES) return json(rpcError(null, -32600, "request too large"), 413);
  const text = await request.text();
  if (text.length > MAX_BODY_BYTES) return json(rpcError(null, -32600, "request too large"), 413);

  let body;
  try {
    body = JSON.parse(text);
  } catch {
    return json(rpcError(null, -32700, "parse error"), 400);
  }

  const batch = Array.isArray(body);
  const calls = batch ? body : [body];
  if (calls.length === 0) return json(rpcError(null, -32600, "empty batch"), 400);
  if (calls.length > MAX_BATCH) return json(rpcError(null, -32600, `batch is capped at ${MAX_BATCH} calls`), 400);

  const refused = calls.map(check);
  if (refused.some((e) => e !== null)) {
    // Refuse the whole request; report each refused call by id.
    const errors = refused.map((e, i) => e ?? rpcError(calls[i].id, -32600, "batch refused: it contains a disallowed call"));
    return json(batch ? errors : errors[0]);
  }

  if (!batch) {
    const answer = local(calls[0]);
    if (answer !== undefined) return json(answer);
  }

  // Re-encoded only when a sync call's timeout was capped; otherwise the
  // origin gets the client's exact bytes.
  let forward = text;
  if (calls.some((c) => c.method === SYNC_METHOD)) {
    const capped = calls.map(capSync);
    forward = JSON.stringify(batch ? capped : capped[0]);
  }

  let upstream;
  try {
    upstream = await fetch(request.url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: forward,
    });
  } catch {
    return json(rpcError(batch ? null : calls[0].id, -32603, "origin unreachable"), 502);
  }
  const headers = new Headers(upstream.headers);
  // The edge owns CORS: drop anything the origin said, then set ours.
  for (const k of [...headers.keys()]) if (k.startsWith("access-control-")) headers.delete(k);
  for (const [k, v] of Object.entries(CORS)) headers.set(k, v);
  return new Response(upstream.body, { status: upstream.status, headers });
}
