// Builds the `sova-miner mine` argument list (pure, so it's unit-tested in
// mineArgs.test.ts without spawning anything).
/**
 * Which Sova node a run's votes come from. An explicit `requested` URL wins;
 * an explicit empty string means "no votes"; omitted means the agent's own
 * node (`ownNode`, from SOVA_NODE_RPC_URL) if one is configured, else none.
 * Never a public default: a vote is only as good as the node it came from,
 * and pointing --sova-rpc at someone else's node hands them the vote.
 */
export function resolveSovaRpcUrl(requested, ownNode) {
    const url = requested ?? ownNode;
    return url && url.trim() !== "" ? url.trim() : undefined;
}
export function buildMineArgs(opts) {
    const args = [
        "--data-dir",
        opts.dataDir,
        "--network",
        opts.network,
        "mine",
        "--budget-zat",
        String(opts.budgetZat),
        "--per-epoch-zat",
        String(opts.perEpochZat),
        "--rpc",
        opts.rpcUrl,
    ];
    if (opts.pollIntervalMs !== undefined) {
        args.push("--poll-interval-ms", String(opts.pollIntervalMs));
    }
    if (opts.maxEpochs !== undefined) {
        args.push("--max-epochs", String(opts.maxEpochs));
    }
    if (opts.sovaRpcUrl !== undefined) {
        args.push("--sova-rpc", opts.sovaRpcUrl);
        if (opts.voteWaitSecs !== undefined) {
            args.push("--vote-wait", String(opts.voteWaitSecs));
        }
    }
    return args;
}
//# sourceMappingURL=mineArgs.js.map