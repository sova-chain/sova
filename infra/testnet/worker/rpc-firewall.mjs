// infra/testnet/worker/rpc-firewall.mjs -- the edge half of "read-and-
// broadcast only, enforced twice" (docs/design/infra-m1.md §2). Runs as a
// Cloudflare Worker on the route rpc.<domain>/*; fetch(request) then goes
// to the origin (the rpc-1 tunnel), whose SOVA_RPC_PROFILE=public enforces
// the same method list again. Rate-limit rules can't read bodies, so the
// method allowlist, batch cap and size cap live here.
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
]);

export const MAX_BODY_BYTES = 64 * 1024;
export const MAX_BATCH = 10;
export const MAX_LOG_RANGE = 1000;
// 82330 = the sova-testnet chain ID (bin/sova/src/chain.rs). Answered here
// without touching the origin.
const CHAIN_ID_HEX = "0x1419a";
const CHAIN_ID_DEC = "82330";

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "POST, OPTIONS",
  "access-control-allow-headers": "content-type",
  "access-control-max-age": "86400",
};

function json(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...CORS },
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
  async fetch(request) {
    if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
    if (request.method !== "POST") {
      return json(rpcError(null, -32600, "POST JSON-RPC to this URL (Sova testnet, chain ID 82330)"), 405);
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

    const upstream = await fetch(request.url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: text,
    });
    const headers = new Headers(upstream.headers);
    for (const [k, v] of Object.entries(CORS)) headers.set(k, v);
    return new Response(upstream.body, { status: upstream.status, headers });
  },
};
