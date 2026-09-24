#!/usr/bin/env node
// sova-miner MCP server (task D3): wraps the sova-miner CLI (D2) so a
// Claude agent can mine SOVA end to end over stdio -- init a keystore, fund
// it on regtest, run the budget-capped mine loop, check on it, stop it, and
// pull a verified report. See mcp/README.md for the registration snippet
// and the zero-to-mining walkthrough, and mcp/docs/walkthrough.md for a
// captured transcript proving the loop end to end.
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { DEFAULT_MINER_DATA_DIR, DEFAULT_RPC_URL, DEFAULT_SOVA_RPC_URL, MINER_BIN, REGTEST_HARNESS_DIR, } from "./paths.js";
import { runMinerOnce } from "./minerCli.js";
import { resolveSovaRpcUrl } from "./mineArgs.js";
import { readMinerState } from "./minerState.js";
import { generateToAddress, getBlockCount } from "./rpc.js";
import { findActiveRun, listRuns, readRunLog, startMineRun, stopRun, } from "./runManager.js";
import { summarizeMineLog } from "./logParse.js";
const NetworkEnum = z
    .enum(["regtest", "test", "testnet", "main", "mainnet"])
    .default("regtest");
function ok(payload) {
    return {
        content: [
            { type: "text", text: JSON.stringify(payload, null, 2) },
        ],
    };
}
function fail(message) {
    return {
        isError: true,
        content: [{ type: "text", text: message }],
    };
}
function errorMessage(err) {
    return err instanceof Error ? err.message : String(err);
}
const server = new McpServer({
    name: "sova-miner-mcp",
    version: "0.1.0",
    title: "Sova Miner",
});
// ---------------------------------------------------------------------
// sova_init
// ---------------------------------------------------------------------
server.registerTool("sova_init", {
    title: "Initialize the sova-miner keystore",
    description: "Runs `sova-miner init`: creates (or loads) this miner's keystore and " +
        "prints the transparent (t-addr) Zcash address to fund, and the EVM " +
        "address burns credit (by default the keystore key's own Ethereum " +
        "address; the user spends that SOVA by running `sova-miner " +
        "export-evm-key --i-understand` themselves and importing the key into " +
        "an EVM wallet -- this server never exports keys). Idempotent -- " +
        "safe to call again; it loads the existing keystore instead of " +
        "regenerating it. Relay any `warnings` to the user: a legacy keystore " +
        "credits an unspendable address until `sova-miner init " +
        "--migrate-evm-address` is run.",
    inputSchema: {
        dataDir: z
            .string()
            .optional()
            .describe(`Directory for keystore.json/state.json (default: ${DEFAULT_MINER_DATA_DIR}).`),
        network: NetworkEnum.describe("Zcash network to build addresses/transactions for. Must match the node behind --rpc later."),
        evmAddress: z
            .string()
            .optional()
            .describe("Credit this EVM address (20-byte hex, with or without 0x) instead of the keystore key's own Ethereum address. Only for an address whose key the user holds."),
    },
}, async ({ dataDir, network, evmAddress }) => {
    const resolvedDataDir = dataDir ?? DEFAULT_MINER_DATA_DIR;
    const args = ["--data-dir", resolvedDataDir, "--network", network, "init"];
    if (evmAddress)
        args.push("--evm-address", evmAddress);
    try {
        const result = await runMinerOnce(args);
        if (result.exitCode !== 0) {
            return fail(`sova-miner init exited ${result.exitCode}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
        }
        const state = await readMinerState(resolvedDataDir);
        return ok({
            dataDir: resolvedDataDir,
            network,
            fundingAddress: state?.address,
            evmAddressHex: state ? `0x${state.evm_address_hex}` : undefined,
            keystorePath: `${resolvedDataDir}/keystore.json`,
            statePath: `${resolvedDataDir}/state.json`,
            cliOutput: result.stdout.trim(),
            warnings: result.stderr.trim() || undefined,
        });
    }
    catch (err) {
        return fail(errorMessage(err));
    }
});
// ---------------------------------------------------------------------
// sova_fund_regtest
// ---------------------------------------------------------------------
server.registerTool("sova_fund_regtest", {
    title: "Fund the miner on regtest (dev-only)",
    description: "Regtest-only convenience: calls the zebrad `generatetoaddress` RPC " +
        "to mine `blocks` coinbase blocks directly to the miner's funding " +
        "address, so its coinbase is spendable immediately. Default is 101 " +
        "blocks, since Zcash coinbase needs 100 confirmations to mature -- " +
        "mining 101 to the SAME address leaves block 1's coinbase with 100 " +
        "confirmations. Do not point this at a real network.",
    inputSchema: {
        dataDir: z
            .string()
            .optional()
            .describe(`Miner data dir to read the funding address from, if --address is omitted (default: ${DEFAULT_MINER_DATA_DIR}).`),
        rpcUrl: z
            .string()
            .default(DEFAULT_RPC_URL)
            .describe("zebrad regtest JSON-RPC endpoint."),
        blocks: z
            .number()
            .int()
            .positive()
            .default(101)
            .describe("Number of blocks to mine to the address (101 = 1 mature coinbase)."),
        address: z
            .string()
            .optional()
            .describe("Override: mine to this t-addr instead of reading it from the miner's state."),
    },
}, async ({ dataDir, rpcUrl, blocks, address }) => {
    try {
        const resolvedDataDir = dataDir ?? DEFAULT_MINER_DATA_DIR;
        let target = address;
        if (!target) {
            const state = await readMinerState(resolvedDataDir);
            if (!state) {
                return fail(`no address given and no miner state found at ${resolvedDataDir}/state.json -- run sova_init first, or pass an explicit address.`);
            }
            target = state.address;
        }
        const beforeTip = await getBlockCount(rpcUrl).catch(() => null);
        const hashes = await generateToAddress(rpcUrl, blocks, target);
        const afterTip = await getBlockCount(rpcUrl);
        return ok({
            rpcUrl,
            fundedAddress: target,
            blocksMined: hashes.length,
            tipHeightBefore: beforeTip,
            tipHeightAfter: afterTip,
            firstBlockHash: hashes[0],
            lastBlockHash: hashes[hashes.length - 1],
        });
    }
    catch (err) {
        return fail(`${errorMessage(err)}\n(is the regtest harness up? see ${REGTEST_HARNESS_DIR}: docker compose up -d)`);
    }
});
// ---------------------------------------------------------------------
// sova_mine
// ---------------------------------------------------------------------
server.registerTool("sova_mine", {
    title: "Start the mine loop",
    description: "Starts `sova-miner mine` as a background child process: while budget " +
        "remains, it submits one SIP-1 burn per new Zcash block it observes " +
        "over RPC. Returns immediately with a run id -- use sova_status to " +
        "watch progress, sova_stop to end it early. Only one run may be " +
        "active at a time. SIP-8 anchored burns: with a Sova node (sovaRpcUrl, " +
        "defaulting to the agent's own node from SOVA_NODE_RPC_URL when set) " +
        "each burn also votes for that node's head once SIP-8 is active on the " +
        "network; it is not active on any network yet, so until then burns stay " +
        "SIP-1 v1 and the log says why. Only use the user's own node: a vote is " +
        "only as good as the node it came from.",
    inputSchema: {
        budgetZat: z
            .number()
            .int()
            .positive()
            .describe("Total zatoshis (burn + fee, across the whole run) this run may spend."),
        perEpochZat: z
            .number()
            .int()
            .positive()
            .describe("Zatoshis burned to the SIP-1 eater script per epoch."),
        dataDir: z
            .string()
            .optional()
            .describe(`Miner data dir (default: ${DEFAULT_MINER_DATA_DIR}).`),
        network: NetworkEnum,
        rpcUrl: z
            .string()
            .default(DEFAULT_RPC_URL)
            .describe("zebrad-compatible JSON-RPC endpoint."),
        pollIntervalMs: z
            .number()
            .int()
            .positive()
            .optional()
            .describe("How often to poll getblockcount, in ms (CLI default 1000)."),
        maxEpochs: z
            .number()
            .int()
            .positive()
            .optional()
            .describe("Stop after this many epochs in this run (default: unbounded)."),
        sovaRpcUrl: z
            .string()
            .optional()
            .describe(`SIP-8: JSON-RPC URL of the user's OWN Sova node, passed as --sova-rpc (default: ${DEFAULT_SOVA_RPC_URL ?? "none -- SOVA_NODE_RPC_URL is not set"}). Pass "" to mine without votes. Never point it at someone else's node: that hands them the vote.`),
        voteWaitSecs: z
            .number()
            .int()
            .min(0)
            .optional()
            .describe("SIP-8: --vote-wait, seconds to wait for the Sova block anchoring a new Zcash block before voting for the head as it is (CLI default 10). Ignored without a Sova node."),
    },
}, async ({ budgetZat, perEpochZat, dataDir, network, rpcUrl, pollIntervalMs, maxEpochs, sovaRpcUrl, voteWaitSecs, }) => {
    try {
        const resolvedDataDir = dataDir ?? DEFAULT_MINER_DATA_DIR;
        const sova = resolveSovaRpcUrl(sovaRpcUrl, DEFAULT_SOVA_RPC_URL);
        const run = await startMineRun({
            dataDir: resolvedDataDir,
            network,
            budgetZat,
            perEpochZat,
            rpcUrl,
            pollIntervalMs,
            maxEpochs,
            sovaRpcUrl: sova,
            voteWaitSecs,
        });
        return ok({
            runId: run.runId,
            pid: run.pid,
            command: run.command,
            sovaRpcUrl: sova ?? null,
            logPath: run.logPath,
            startedAt: run.startedAt,
            note: "call sova_status with this runId to watch progress.",
        });
    }
    catch (err) {
        return fail(errorMessage(err));
    }
});
// ---------------------------------------------------------------------
// sova_status
// ---------------------------------------------------------------------
server.registerTool("sova_status", {
    title: "Check on a mine run",
    description: "Tails a mine run's log and summarizes progress (epochs completed, " +
        "last epoch's burn/fee/txid, whether/why it stopped). Defaults to " +
        "the most recently started run if runId is omitted.",
    inputSchema: {
        runId: z
            .string()
            .optional()
            .describe("Run id from sova_mine. Defaults to the most recent run."),
        tailLines: z
            .number()
            .int()
            .positive()
            .default(40)
            .describe("How many lines of raw log tail to include."),
    },
}, async ({ runId, tailLines }) => {
    try {
        const { run, content } = await readRunLog(runId);
        const summary = summarizeMineLog(content);
        const lines = content.split("\n");
        const tail = lines.slice(Math.max(0, lines.length - tailLines)).join("\n");
        return ok({
            runId: run.runId,
            status: run.status,
            pid: run.pid,
            startedAt: run.startedAt,
            endedAt: run.endedAt,
            exitCode: run.exitCode,
            exitSignal: run.exitSignal,
            ...summary,
            logPath: run.logPath,
            logTail: tail,
        });
    }
    catch (err) {
        return fail(errorMessage(err));
    }
});
// ---------------------------------------------------------------------
// sova_stop
// ---------------------------------------------------------------------
server.registerTool("sova_stop", {
    title: "Stop a mine run",
    description: "Stops a running mine loop (SIGTERM, escalating to SIGKILL after a " +
        "grace period if needed). No-op if the run has already ended. " +
        "Defaults to the most recently started run if runId is omitted.",
    inputSchema: {
        runId: z.string().optional(),
        graceMs: z
            .number()
            .int()
            .positive()
            .default(5000)
            .describe("How long to wait for a clean SIGTERM exit before SIGKILL."),
    },
}, async ({ runId, graceMs }) => {
    try {
        const run = await stopRun(runId, graceMs);
        return ok({
            runId: run.runId,
            status: run.status,
            exitCode: run.exitCode,
            exitSignal: run.exitSignal,
            endedAt: run.endedAt,
        });
    }
    catch (err) {
        return fail(errorMessage(err));
    }
});
// ---------------------------------------------------------------------
// sova_report
// ---------------------------------------------------------------------
server.registerTool("sova_report", {
    title: "Print the earnings/spend report",
    description: "Runs `sova-miner report`, optionally with --verify-rpc to independently " +
        "re-scan the chain for this miner's SIP-1 burns and confirm the count " +
        "and total zatoshis match the local state exactly (txid-set diff, not " +
        "just totals).",
    inputSchema: {
        dataDir: z
            .string()
            .optional()
            .describe(`Miner data dir (default: ${DEFAULT_MINER_DATA_DIR}).`),
        network: NetworkEnum,
        verifyRpcUrl: z
            .string()
            .optional()
            .describe("If given, cross-check the report against this node's full chain history."),
    },
}, async ({ dataDir, network, verifyRpcUrl }) => {
    const resolvedDataDir = dataDir ?? DEFAULT_MINER_DATA_DIR;
    const args = ["--data-dir", resolvedDataDir, "--network", network, "report"];
    if (verifyRpcUrl)
        args.push("--verify-rpc", verifyRpcUrl);
    try {
        const result = await runMinerOnce(args, 120_000);
        const matches = /MATCH:\s*yes/.test(result.stdout);
        const payload = {
            dataDir: resolvedDataDir,
            verified: Boolean(verifyRpcUrl),
            matches: verifyRpcUrl ? matches : undefined,
            exitCode: result.exitCode,
            report: result.stdout.trim(),
        };
        return result.exitCode === 0 ? ok(payload) : fail(JSON.stringify(payload, null, 2));
    }
    catch (err) {
        return fail(errorMessage(err));
    }
});
async function main() {
    const transport = new StdioServerTransport();
    await server.connect(transport);
    // stdout is the MCP transport channel -- all diagnostics go to stderr.
    console.error(`sova-miner-mcp: connected over stdio (binary: ${MINER_BIN}, active run: ${findActiveRun()?.runId ?? "none"}, known runs: ${listRuns().length})`);
}
main().catch((err) => {
    console.error("sova-miner-mcp: fatal error", err);
    process.exit(1);
});
//# sourceMappingURL=index.js.map