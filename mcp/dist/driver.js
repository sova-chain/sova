#!/usr/bin/env node
// Driver script for task D3's "prove the loop" acceptance criterion: speaks
// real MCP protocol (over stdio, via @modelcontextprotocol/sdk's Client) to
// the built server, exercising every tool end to end against a live
// regtest harness. This stands in for a Claude conversation -- same
// protocol, same tool schemas, just a scripted client instead of a model
// choosing the calls -- and its transcript is captured into
// mcp/docs/walkthrough.md by mcp/scripts/run-walkthrough.sh.
//
// Usage: node dist/driver.js
//   (build first: npm run build)
//
// Exits non-zero if any tool call errors or an expectation isn't met.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import path from "node:path";
import { fileURLToPath } from "node:url";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const SERVER_ENTRY = path.join(HERE, "index.js"); // dist/index.js next to dist/driver.js
function log(section, payload) {
    console.log(`\n### ${section}\n`);
    console.log(typeof payload === "string" ? payload : JSON.stringify(payload, null, 2));
}
function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
}
async function callTool(client, name, args) {
    console.log(`\n>>> tool_call: ${name}(${JSON.stringify(args)})`);
    const result = await client.callTool({ name, arguments: args });
    const content = result
        .content ?? [];
    const text = content.map((c) => c.text ?? "").join("\n");
    let json = undefined;
    try {
        json = JSON.parse(text);
    }
    catch {
        // not JSON (e.g. a plain error string) -- fine, keep raw text.
    }
    const isError = Boolean(result.isError);
    console.log(`<<< result (isError=${isError}):`);
    console.log(text);
    return { isError, text, json };
}
async function main() {
    const rpcUrl = process.env.SOVA_REGTEST_RPC_URL ?? "http://127.0.0.1:18232";
    const dataDir = process.env.SOVA_DRIVER_DATA_DIR ??
        path.join(HERE, "..", ".data", "driver-miner");
    console.log("=== sova-miner MCP driver: zero-to-mining walkthrough ===");
    console.log(`server entry: ${SERVER_ENTRY}`);
    console.log(`rpc url:      ${rpcUrl}`);
    console.log(`data dir:     ${dataDir}`);
    const inheritedEnv = Object.fromEntries(Object.entries(process.env).filter((entry) => entry[1] !== undefined));
    const transport = new StdioClientTransport({
        command: process.execPath,
        args: [SERVER_ENTRY],
        env: { ...inheritedEnv, SOVA_MCP_DATA_DIR: path.dirname(dataDir) },
    });
    const client = new Client({ name: "sova-mcp-driver", version: "0.1.0" }, {});
    await client.connect(transport);
    const { tools } = await client.listTools();
    log("tools/list", tools.map((t) => ({ name: t.name, description: t.description })));
    // 1. init
    const init = await callTool(client, "sova_init", { dataDir, network: "regtest" });
    if (init.isError)
        throw new Error("sova_init failed");
    const initJson = init.json;
    const fundingAddress = initJson?.fundingAddress;
    if (!fundingAddress)
        throw new Error("sova_init did not return a fundingAddress");
    console.log(`\n>>> derived funding address: ${fundingAddress}`);
    // 2. fund on regtest
    const fund = await callTool(client, "sova_fund_regtest", {
        dataDir,
        rpcUrl,
        blocks: 101,
    });
    if (fund.isError)
        throw new Error("sova_fund_regtest failed");
    // 3. start background auto-mining so the miner has new blocks to react to
    //    (mirrors box/regtest/auto-mine.sh; the miner only reacts to blocks,
    //    it never mines them itself -- see mine.rs's design note).
    const { spawn } = await import("node:child_process");
    const autoMine = spawn(path.join(HERE, "..", "..", "box", "regtest", "auto-mine.sh"), ["2", rpcUrl], { stdio: "ignore" });
    console.log(`\n>>> started auto-mine.sh (pid ${autoMine.pid}), 1 block/2s`);
    try {
        // 4. mine
        const mine = await callTool(client, "sova_mine", {
            dataDir,
            network: "regtest",
            rpcUrl,
            budgetZat: 3_000_000,
            perEpochZat: 100_000,
            pollIntervalMs: 300,
            maxEpochs: 5,
        });
        if (mine.isError)
            throw new Error("sova_mine failed");
        const runId = mine.json?.runId;
        if (!runId)
            throw new Error("sova_mine did not return a runId");
        // 4a. "one active run at a time": a second sova_mine call while this one
        // is still going must be refused, not silently start a second process.
        console.log("\n>>> (exercising the single-active-run guard: calling sova_mine again while the above run is still active)");
        const rejected = await callTool(client, "sova_mine", {
            dataDir,
            network: "regtest",
            rpcUrl,
            budgetZat: 100_000,
            perEpochZat: 50_000,
        });
        if (!rejected.isError) {
            throw new Error("expected sova_mine to refuse a second concurrent run, but it did not error");
        }
        console.log(">>> confirmed: second concurrent sova_mine call was refused, as expected.");
        // 5. poll status until the run ends (max-epochs reached or error)
        let statusJson;
        for (let attempt = 0; attempt < 60; attempt += 1) {
            await sleep(2000);
            const status = await callTool(client, "sova_status", { runId, tailLines: 30 });
            statusJson = status.json;
            if (statusJson?.status !== "running")
                break;
        }
        if (!statusJson || statusJson.status === "running") {
            throw new Error("mine run did not finish within the polling window");
        }
        if (statusJson.epochsCompleted < 5) {
            throw new Error(`expected 5 epochs completed, got ${statusJson.epochsCompleted}`);
        }
        // 6. stop (no-op if already exited -- proves the tool handles that path)
        const stop = await callTool(client, "sova_stop", { runId });
        if (stop.isError)
            throw new Error("sova_stop failed");
        // 7. report, verified against chain
        const report = await callTool(client, "sova_report", {
            dataDir,
            network: "regtest",
            verifyRpcUrl: rpcUrl,
        });
        if (report.isError)
            throw new Error("sova_report failed");
        const reportJson = report.json;
        if (reportJson?.matches !== true) {
            throw new Error("sova_report --verify-rpc did not report a match");
        }
        console.log("\n=== WALKTHROUGH PASSED: init -> fund -> mine -> status -> stop -> report (verified) ===");
        // 8. exercise sova_stop against a run that is still ACTUALLY RUNNING
        // (the sova_stop call above only exercised the already-exited/no-op
        // path, since that run had already finished via --max-epochs).
        console.log("\n### phase 2: stopping a live run mid-flight (sova_stop's real path, not the no-op-if-exited path)\n");
        const mine2 = await callTool(client, "sova_mine", {
            dataDir,
            network: "regtest",
            rpcUrl,
            budgetZat: 1_000_000,
            perEpochZat: 100_000,
            pollIntervalMs: 300,
            // deliberately no maxEpochs -- this run only stops when we tell it to.
        });
        if (mine2.isError)
            throw new Error("sova_mine (phase 2) failed");
        const runId2 = mine2.json?.runId;
        if (!runId2)
            throw new Error("sova_mine (phase 2) did not return a runId");
        // Let it complete at least one epoch (auto-mine.sh is still producing a
        // block every 2s in the background) before stopping it.
        let sawRunning = false;
        let sawEpoch = false;
        for (let attempt = 0; attempt < 15; attempt += 1) {
            await sleep(1500);
            const status = await callTool(client, "sova_status", { runId: runId2, tailLines: 10 });
            const j = status.json;
            if (j.status === "running")
                sawRunning = true;
            if ((j.epochsCompleted ?? 0) >= 1) {
                sawEpoch = true;
                break;
            }
        }
        if (!sawRunning)
            throw new Error("phase 2 run was never observed in 'running' status");
        if (!sawEpoch)
            throw new Error("phase 2 run never completed an epoch before timeout");
        const stop2 = await callTool(client, "sova_stop", { runId: runId2, graceMs: 5000 });
        if (stop2.isError)
            throw new Error("sova_stop (phase 2) failed");
        const stop2Json = stop2.json;
        if (stop2Json.status !== "stopped") {
            throw new Error(`expected phase 2 run to be in status "stopped" after sova_stop, got ${stop2Json.status}`);
        }
        const finalStatus = await callTool(client, "sova_status", { runId: runId2 });
        const finalJson = finalStatus.json;
        if (finalJson.status !== "stopped") {
            throw new Error(`expected final status "stopped", got ${finalJson.status}`);
        }
        console.log(`\n>>> confirmed: sova_stop terminated a live run (exitSignal=${finalJson.exitSignal}).`);
        // sova_stop again should be a safe no-op on an already-stopped run.
        const stopAgain = await callTool(client, "sova_stop", { runId: runId2 });
        if (stopAgain.isError)
            throw new Error("sova_stop (idempotent re-stop) failed");
        console.log("\n=== PHASE 2 PASSED: sova_mine -> sova_status(running) -> sova_stop (live) -> sova_status(stopped) ===");
    }
    finally {
        autoMine.kill("SIGTERM");
    }
    await client.close();
}
main().catch((err) => {
    console.error("\n=== WALKTHROUGH FAILED ===");
    console.error(err);
    process.exit(1);
});
//# sourceMappingURL=driver.js.map