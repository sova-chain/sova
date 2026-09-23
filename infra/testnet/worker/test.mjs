// Offline tests for rpc-firewall.mjs (Node 18+: global Request/Response).
//   node infra/testnet/worker/test.mjs
// Also checks the allowlist equals bin/sova/src/rpc.rs PUBLIC_RPC_METHODS.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import worker, { ALLOWED, MAX_BATCH } from "./rpc-firewall.mjs";

const here = dirname(fileURLToPath(import.meta.url));
let upstreamCalls = 0;
globalThis.fetch = async (_url, init) => {
  upstreamCalls += 1;
  const body = JSON.parse(init.body);
  const answer = (c) => ({ jsonrpc: "2.0", id: c.id, result: "0x1" });
  return new Response(JSON.stringify(Array.isArray(body) ? body.map(answer) : answer(body)), {
    headers: { "content-type": "application/json" },
  });
};

const URL_ = "https://rpc.testnet.example/";
const post = (body) =>
  worker.fetch(new Request(URL_, { method: "POST", headers: { "content-type": "application/json" }, body: typeof body === "string" ? body : JSON.stringify(body) }));
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
  await t(`denies ${m}`, async () => {
    const before = upstreamCalls;
    const r = await (await post(call(m))).json();
    assert.equal(r.error.code, -32601);
    assert.equal(upstreamCalls, before, "never reaches the origin");
  });
}

await t("forwards an allowed call", async () => {
  const before = upstreamCalls;
  const r = await (await post(call("eth_blockNumber"))).json();
  assert.equal(r.result, "0x1");
  assert.equal(upstreamCalls, before + 1);
});
await t("answers eth_chainId at the edge (82330)", async () => {
  const before = upstreamCalls;
  const r = await (await post(call("eth_chainId"))).json();
  assert.equal(Number.parseInt(r.result, 16), 82330);
  assert.equal(upstreamCalls, before);
});
await t("batch of allowed calls is forwarded", async () => {
  const r = await (await post([call("eth_blockNumber", [], 1), call("eth_gasPrice", [], 2)])).json();
  assert.equal(r.length, 2);
});
await t("batch with one denied call is refused whole", async () => {
  const before = upstreamCalls;
  const r = await (await post([call("eth_blockNumber", [], 1), call("debug_traceCall", [], 2)])).json();
  assert.equal(r[1].error.code, -32601);
  assert.ok(r[0].error);
  assert.equal(upstreamCalls, before);
});
await t(`batch over ${MAX_BATCH} refused`, async () => {
  const res = await post(Array.from({ length: MAX_BATCH + 1 }, (_, i) => call("eth_blockNumber", [], i)));
  assert.equal(res.status, 400);
});
await t("empty batch refused", async () => assert.equal((await post([])).status, 400));
await t("oversized body refused", async () => {
  const res = await post(JSON.stringify(call("eth_call", [{ data: "0x" + "00".repeat(40000) }])));
  assert.equal(res.status, 413);
});
await t("parse error", async () => assert.equal((await post("{not json")).status, 400));
await t("GET is 405", async () => assert.equal((await worker.fetch(new Request(URL_))).status, 405));
await t("OPTIONS preflight has CORS", async () => {
  const res = await worker.fetch(new Request(URL_, { method: "OPTIONS" }));
  assert.equal(res.status, 204);
  assert.equal(res.headers.get("access-control-allow-origin"), "*");
});
await t("eth_getLogs range cap", async () => {
  const r = await (await post(call("eth_getLogs", [{ fromBlock: "0x1", toBlock: "0x5000" }]))).json();
  assert.equal(r.error.code, -32005);
  const ok = await (await post(call("eth_getLogs", [{ fromBlock: "0x1", toBlock: "0x10" }]))).json();
  assert.equal(ok.result, "0x1");
});
console.log(`1..${n} all passed`);
