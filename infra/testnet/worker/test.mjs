// Offline tests for rpc-firewall.mjs (Node 18+: global Request/Response).
//   node infra/testnet/worker/test.mjs
// Also checks the allowlist equals bin/sova/src/rpc.rs PUBLIC_RPC_METHODS.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import worker, { ALLOWED, CORS, MAX_BATCH } from "./rpc-firewall.mjs";

const here = dirname(fileURLToPath(import.meta.url));
let upstreamCalls = 0;
// How the fake origin behaves: "ok", "throw" (unreachable), "500", or
// "cors" (the origin sends CORS headers of its own).
let upstreamMode = "ok";
globalThis.fetch = async (_url, init) => {
  upstreamCalls += 1;
  if (upstreamMode === "throw") throw new TypeError("connection refused");
  const body = JSON.parse(init.body);
  const answer = (c) => ({ jsonrpc: "2.0", id: c.id, result: "0x1" });
  const headers = { "content-type": "application/json" };
  if (upstreamMode === "cors") {
    headers["access-control-allow-origin"] = "https://evil.example";
    headers["access-control-allow-credentials"] = "true";
  }
  return new Response(JSON.stringify(Array.isArray(body) ? body.map(answer) : answer(body)), {
    status: upstreamMode === "500" ? 500 : 200,
    headers,
  });
};

const URL_ = "https://rpc.testnet.example/";
const ORIGIN = "https://sova.io";

// A stand-in for the Workers Rate Limiting binding: a fixed window per key
// that never rolls over (tests are faster than 10 s). Records every key.
function mockLimiter(limit) {
  const counts = new Map();
  return {
    keys: [],
    async limit({ key }) {
      this.keys.push(key);
      const n = (counts.get(key) ?? 0) + 1;
      counts.set(key, n);
      return { success: n <= limit };
    },
  };
}
// The env cloudflare.sh deploys (binding + plain-text vars), with a limit
// the ordinary tests never reach.
const deployedEnv = (limiter) => ({ RPC_RATELIMIT: limiter, RPC_RATELIMIT_REQUESTS: "50", RPC_RATELIMIT_PERIOD: "10" });
const roomyEnv = deployedEnv(mockLimiter(1e9));

// Every request carries an Origin, as a browser's would, and a client IP,
// as Cloudflare's edge sets it.
const req = (body, { ip = "203.0.113.7", method = "POST" } = {}) =>
  new Request(URL_, {
    method,
    headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": ip },
    body: method === "POST" ? (typeof body === "string" ? body : JSON.stringify(body)) : undefined,
  });
const post = (body, env = roomyEnv, opts = {}) => worker.fetch(req(body, opts), env);
const preflight = (env, ip = "203.0.113.7") =>
  worker.fetch(
    new Request(URL_, {
      method: "OPTIONS",
      headers: { origin: ORIGIN, "cf-connecting-ip": ip, "access-control-request-method": "POST", "access-control-request-headers": "content-type" },
    }),
    env,
  );
// console.warn lines a block logs.
async function warnsDuring(fn) {
  const seen = [];
  const orig = console.warn;
  console.warn = (...a) => seen.push(a.join(" "));
  try {
    await fn();
  } finally {
    console.warn = orig;
  }
  return seen;
}
// Every response a browser can see must carry Access-Control-Allow-Origin,
// or the page gets an opaque network error instead of the JSON-RPC error.
const cors = (res) => {
  assert.equal(res.headers.get("access-control-allow-origin"), "*", `status ${res.status} lacks CORS`);
  return res;
};
const postJson = async (body) => (await cors(await post(body))).json();
const call = (method, params = [], id = 1) => ({ jsonrpc: "2.0", id, method, params });
let n = 0;
async function t(name, fn) {
  await fn();
  n += 1;
  console.log(`ok ${n} ${name}`);
}

// Same list as the node's public profile.
const rs = readFileSync(join(here, "../../../bin/sova/src/rpc.rs"), "utf8");
const start = rs.indexOf("const PUBLIC_RPC_METHODS");
assert.ok(start > 0, "PUBLIC_RPC_METHODS not found in rpc.rs");
const block = rs.slice(start, rs.indexOf("];", start));
const nodeList = [...block.matchAll(/"([a-zA-Z0-9_]+)"/g)].map((m) => m[1]).sort();
await t("allowlist equals bin/sova PUBLIC_RPC_METHODS", () => assert.deepEqual([...ALLOWED].sort(), nodeList));

for (const m of ["admin_nodeInfo", "debug_traceTransaction", "trace_block", "txpool_content", "engine_newPayloadV4", "personal_sign", "miner_start", "eth_sign", "eth_sendTransaction", "eth_newFilter", "eth_subscribe", "ots_getApiLevel", "eth_accounts"]) {
  await t(`denies ${m} (with CORS)`, async () => {
    const before = upstreamCalls;
    const r = await postJson(call(m));
    assert.equal(r.error.code, -32601);
    assert.equal(upstreamCalls, before, "never reaches the origin");
  });
}

await t("forwards an allowed call (with CORS)", async () => {
  const before = upstreamCalls;
  const r = await postJson(call("eth_blockNumber"));
  assert.equal(r.result, "0x1");
  assert.equal(upstreamCalls, before + 1);
});
await t("answers eth_chainId at the edge (82330, with CORS)", async () => {
  const before = upstreamCalls;
  const r = await postJson(call("eth_chainId"));
  assert.equal(Number.parseInt(r.result, 16), 82330);
  assert.equal(upstreamCalls, before);
});
await t("net_version at the edge has CORS", async () => assert.equal((await postJson(call("net_version"))).result, "82330"));
await t("batch of allowed calls is forwarded (with CORS)", async () => {
  const r = await postJson([call("eth_blockNumber", [], 1), call("eth_gasPrice", [], 2)]);
  assert.equal(r.length, 2);
});
await t("batch with one denied call is refused whole (with CORS)", async () => {
  const before = upstreamCalls;
  const r = await postJson([call("eth_blockNumber", [], 1), call("debug_traceCall", [], 2)]);
  assert.equal(r[1].error.code, -32601);
  assert.ok(r[0].error);
  assert.equal(upstreamCalls, before);
});
await t(`batch over ${MAX_BATCH} refused (400, with CORS)`, async () => {
  const res = cors(await post(Array.from({ length: MAX_BATCH + 1 }, (_, i) => call("eth_blockNumber", [], i))));
  assert.equal(res.status, 400);
});
await t("empty batch refused (400, with CORS)", async () => assert.equal(cors(await post([])).status, 400));
await t("oversized body refused (413, with CORS)", async () => {
  const res = cors(await post(JSON.stringify(call("eth_call", [{ data: "0x" + "00".repeat(40000) }]))));
  assert.equal(res.status, 413);
});
await t("declared oversized body refused (413, with CORS)", async () => {
  const req = new Request(URL_, { method: "POST", headers: { "content-length": "999999", origin: ORIGIN }, body: "{}" });
  const res = cors(await worker.fetch(req, roomyEnv));
  assert.equal(res.status, 413);
});
await t("parse error (400, with CORS)", async () => assert.equal(cors(await post("{not json")).status, 400));
await t("GET is 405 (with CORS and Allow)", async () => {
  const res = cors(await worker.fetch(new Request(URL_, { headers: { origin: ORIGIN } }), roomyEnv));
  assert.equal(res.status, 405);
  assert.equal(res.headers.get("allow"), "POST, OPTIONS");
});
await t("OPTIONS preflight: 204 with the full CORS set", async () => {
  const res = await worker.fetch(
    new Request(URL_, {
      method: "OPTIONS",
      headers: { origin: ORIGIN, "access-control-request-method": "POST", "access-control-request-headers": "content-type" },
    }),
  );
  assert.equal(res.status, 204);
  assert.equal(res.headers.get("access-control-allow-origin"), "*");
  assert.equal(res.headers.get("access-control-allow-methods"), "POST, OPTIONS");
  assert.equal(res.headers.get("access-control-allow-headers"), "content-type");
  const maxAge = Number(res.headers.get("access-control-max-age"));
  assert.ok(maxAge >= 600 && maxAge <= 86400, `max-age ${maxAge}`);
  assert.equal(await res.text(), "");
});
await t("preflight never reaches the origin", async () => {
  const before = upstreamCalls;
  await worker.fetch(new Request(URL_, { method: "OPTIONS", headers: { origin: ORIGIN } }), roomyEnv);
  assert.equal(upstreamCalls, before);
});
await t("no credentials: CORS never allows them", () => assert.equal(CORS["access-control-allow-credentials"], undefined));
await t("eth_getLogs range cap (with CORS)", async () => {
  const r = await postJson(call("eth_getLogs", [{ fromBlock: "0x1", toBlock: "0x5000" }]));
  assert.equal(r.error.code, -32005);
  const ok = await postJson(call("eth_getLogs", [{ fromBlock: "0x1", toBlock: "0x10" }]));
  assert.equal(ok.result, "0x1");
});
await t("origin 500 passes through with CORS", async () => {
  upstreamMode = "500";
  try {
    assert.equal(cors(await post(call("eth_blockNumber"))).status, 500);
  } finally {
    upstreamMode = "ok";
  }
});
await t("origin unreachable is a 502 JSON-RPC error with CORS", async () => {
  upstreamMode = "throw";
  try {
    const res = cors(await post(call("eth_blockNumber", [], 7)));
    assert.equal(res.status, 502);
    const r = await res.json();
    assert.equal(r.id, 7);
    assert.equal(r.error.code, -32603);
  } finally {
    upstreamMode = "ok";
  }
});
await t("origin's own CORS headers are replaced by the edge's", async () => {
  upstreamMode = "cors";
  try {
    const res = cors(await post(call("eth_blockNumber")));
    assert.equal(res.headers.get("access-control-allow-credentials"), null);
    assert.equal(res.headers.get("access-control-allow-methods"), "POST, OPTIONS");
  } finally {
    upstreamMode = "ok";
  }
});
await t("an exception inside the Worker is a 500 with CORS", async () => {
  const broken = {
    method: "POST",
    url: URL_,
    headers: new Headers({ origin: ORIGIN }),
    text: async () => {
      throw new Error("body stream broke");
    },
  };
  const res = cors(await worker.fetch(broken, roomyEnv));
  assert.equal(res.status, 500);
  assert.equal((await res.json()).error.code, -32603);
});

// ---- Per-IP rate limit (the RPC_RATELIMIT binding) ----
const over = async (env, ip) => {
  for (let i = 0; i < 3; i += 1) assert.equal(cors(await post(call("eth_blockNumber"), env, { ip })).status, 200);
  return post(call("eth_blockNumber", [], 9), env, { ip });
};
await t("over the limit: 429 JSON-RPC error with CORS, exposed Retry-After, origin untouched", async () => {
  const env = deployedEnv(mockLimiter(3));
  const before = upstreamCalls;
  const res = cors(await over(env, "198.51.100.1"));
  assert.equal(upstreamCalls, before + 3, "the 4th request never reaches the origin");
  assert.equal(res.status, 429);
  assert.equal(res.headers.get("retry-after"), "10");
  assert.match(res.headers.get("access-control-expose-headers"), /retry-after/i);
  assert.equal(res.headers.get("content-type"), "application/json");
  const r = await res.json();
  assert.equal(r.jsonrpc, "2.0");
  assert.equal(r.id, null);
  assert.equal(r.error.code, -32005);
  assert.match(r.error.message, /50 requests per 10 s per IP/);
});
await t("Retry-After follows RPC_RATELIMIT_PERIOD (60 s binding)", async () => {
  const env = { ...deployedEnv(mockLimiter(0)), RPC_RATELIMIT_PERIOD: "60" };
  const res = cors(await post(call("eth_blockNumber"), env));
  assert.equal(res.status, 429);
  assert.equal(res.headers.get("retry-after"), "60");
});
await t("an over-limit batch, GET or oversized body is also 429 (counted before the body)", async () => {
  const env = deployedEnv(mockLimiter(0));
  assert.equal(cors(await post([call("eth_blockNumber", [], 1), call("eth_gasPrice", [], 2)], env)).status, 429);
  assert.equal(cors(await worker.fetch(req(null, { method: "GET" }), env)).status, 429);
  assert.equal(cors(await post("x".repeat(70000), env)).status, 429);
});
await t("OPTIONS preflights are not counted (and still 204 over the limit)", async () => {
  const limiter = mockLimiter(3);
  const env = deployedEnv(limiter);
  for (let i = 0; i < 20; i += 1) assert.equal((await preflight(env)).status, 204);
  assert.equal(limiter.keys.length, 0, "the limiter never saw a preflight");
  assert.equal(cors(await over(env, "203.0.113.7")).status, 429);
  const pf = await preflight(env);
  assert.equal(pf.status, 204, "a limited IP can still preflight");
  assert.equal(pf.headers.get("access-control-allow-origin"), "*");
});
await t("the key is the client IP (CF-Connecting-IP); other IPs are unaffected", async () => {
  const limiter = mockLimiter(3);
  const env = deployedEnv(limiter);
  assert.equal((await over(env, "198.51.100.2")).status, 429);
  assert.equal(cors(await post(call("eth_blockNumber"), env, { ip: "2001:db8::1" })).status, 200);
  assert.deepEqual([...new Set(limiter.keys)], ["198.51.100.2", "2001:db8::1"]);
});
await t("RPC_RATELIMIT_AT=waf without a binding: forwarded, nothing logged", async () => {
  const before = upstreamCalls;
  const logged = await warnsDuring(async () => {
    assert.equal(cors(await post(call("eth_blockNumber"), { RPC_RATELIMIT_AT: "waf" })).status, 200);
  });
  assert.equal(upstreamCalls, before + 1);
  assert.deepEqual(logged, []);
});
await t("binding missing: fails open, warns once per isolate", async () => {
  const before = upstreamCalls;
  const logged = await warnsDuring(async () => {
    for (const env of [{}, undefined, {}]) assert.equal(cors(await post(call("eth_blockNumber"), env)).status, 200);
  });
  assert.equal(upstreamCalls, before + 3);
  assert.equal(logged.length, 1);
  assert.match(logged[0], /no RPC_RATELIMIT binding.*fail open/);
});
await t("limiter throws: fails open with a warning", async () => {
  const env = deployedEnv({ limit: async () => { throw new Error("limiter down"); } });
  const logged = await warnsDuring(async () => assert.equal(cors(await post(call("eth_blockNumber"), env)).status, 200));
  assert.equal(logged.length, 1);
  assert.match(logged[0], /failing open.*limiter down/);
});
console.log(`1..${n} all passed`);
