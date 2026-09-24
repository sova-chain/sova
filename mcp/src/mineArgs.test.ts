// `npm test`: the `sova-miner mine` argument list the sova_mine tool builds.

import assert from "node:assert/strict";
import { test } from "node:test";

import { buildMineArgs, resolveSovaRpcUrl } from "./mineArgs.js";

const base = {
  dataDir: "/d",
  network: "regtest",
  budgetZat: 2_000_000,
  perEpochZat: 100_000,
  rpcUrl: "http://127.0.0.1:18232",
};

test("without a Sova node the args are exactly the pre-SIP-8 ones", () => {
  assert.deepEqual(buildMineArgs(base), [
    "--data-dir",
    "/d",
    "--network",
    "regtest",
    "mine",
    "--budget-zat",
    "2000000",
    "--per-epoch-zat",
    "100000",
    "--rpc",
    "http://127.0.0.1:18232",
  ]);
  // --vote-wait alone is dropped: it means nothing without --sova-rpc.
  assert.deepEqual(buildMineArgs({ ...base, voteWaitSecs: 3 }), buildMineArgs(base));
});

test("--sova-rpc and --vote-wait are passed through", () => {
  const args = buildMineArgs({
    ...base,
    maxEpochs: 5,
    sovaRpcUrl: "http://127.0.0.1:8545",
    voteWaitSecs: 0,
  });
  assert.deepEqual(args.slice(-6), [
    "--max-epochs",
    "5",
    "--sova-rpc",
    "http://127.0.0.1:8545",
    "--vote-wait",
    "0",
  ]);
  // No --vote-wait given: the CLI's default (10 s) applies.
  const noWait = buildMineArgs({ ...base, sovaRpcUrl: "http://127.0.0.1:8545" });
  assert.deepEqual(noWait.slice(-2), ["--sova-rpc", "http://127.0.0.1:8545"]);
});

test("the Sova node defaults to the agent's own, and only to it", () => {
  const own = "http://127.0.0.1:8545";
  assert.equal(resolveSovaRpcUrl(undefined, own), own);
  assert.equal(resolveSovaRpcUrl(undefined, undefined), undefined);
  assert.equal(resolveSovaRpcUrl(undefined, "  "), undefined);
  assert.equal(resolveSovaRpcUrl("http://10.0.0.5:8545", own), "http://10.0.0.5:8545");
  // An explicit empty string opts out even when an own node is configured.
  assert.equal(resolveSovaRpcUrl("", own), undefined);
});
