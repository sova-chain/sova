// Builds the `sova-miner mine` argument list (pure, so it's unit-tested in
// mineArgs.test.ts without spawning anything).

export interface StartMineArgs {
  dataDir: string;
  network: string;
  budgetZat: number;
  perEpochZat: number;
  rpcUrl: string;
  pollIntervalMs?: number;
  maxEpochs?: number;
  /**
   * SIP-8 anchored burns: the Sova node whose head each burn votes for
   * (`--sova-rpc`). Already resolved (see resolveSovaRpcUrl): undefined
   * means no `--sova-rpc`, i.e. v1 burns that never wait on Sova.
   */
  sovaRpcUrl?: string;
  /** `--vote-wait` in seconds; only passed along with sovaRpcUrl. */
  voteWaitSecs?: number;
}

/**
 * Which Sova node a run's votes come from. An explicit `requested` URL wins;
 * an explicit empty string means "no votes"; omitted means the agent's own
 * node (`ownNode`, from SOVA_NODE_RPC_URL) if one is configured, else none.
 * Never a public default: a vote is only as good as the node it came from,
 * and pointing --sova-rpc at someone else's node hands them the vote.
 */
export function resolveSovaRpcUrl(
  requested: string | undefined,
  ownNode: string | undefined,
): string | undefined {
  const url = requested ?? ownNode;
  return url && url.trim() !== "" ? url.trim() : undefined;
}

export function buildMineArgs(opts: StartMineArgs): string[] {
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
