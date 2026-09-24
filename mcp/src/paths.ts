// Filesystem layout for the sova-miner MCP server.
//
// This package lives at `mcp/` in the sova-chain repo (a deviation, approved
// for task D3, from the workplan's original sketch of a separate repo -- see
// mcp/README.md's "Deviation from the plan" note). Paths below are resolved
// relative to this compiled file's location (mcp/dist/paths.js) so the
// server works no matter what directory it's launched from -- which matters
// because Claude Code / claude mcp add launches stdio servers with an
// arbitrary cwd.

import { fileURLToPath } from "node:url";
import path from "node:path";

// mcp/dist/paths.js -> mcp/dist -> mcp
const MCP_DIR = path.resolve(fileURLToPath(import.meta.url), "..", "..");
// mcp -> repo root
const REPO_ROOT = path.resolve(MCP_DIR, "..");

/** Root of the sova-chain repo checkout (parent of `mcp/`). */
export const REPO_ROOT_DIR = process.env.SOVA_REPO_ROOT ?? REPO_ROOT;

/** The built `sova-miner` binary (crates/burn-wallet is a nested workspace). */
export const MINER_BIN =
  process.env.SOVA_MINER_BIN ??
  path.join(
    REPO_ROOT_DIR,
    "crates",
    "burn-wallet",
    "target",
    "release",
    "sova-miner",
  );

/** The nested cargo workspace `sova-miner` builds from, for error hints. */
export const BURN_WALLET_DIR = path.join(REPO_ROOT_DIR, "crates", "burn-wallet");

/** The regtest harness (docker compose + zebrad), for error hints. */
export const REGTEST_HARNESS_DIR = path.join(REPO_ROOT_DIR, "box", "regtest");

/** Gitignored scratch dir for keystores, state sidecars, and run logs. */
export const DATA_ROOT =
  process.env.SOVA_MCP_DATA_DIR ?? path.join(MCP_DIR, ".data");

/** Default `--data-dir` for the miner identity this server drives. */
export const DEFAULT_MINER_DATA_DIR = path.join(DATA_ROOT, "miner");

/** Where per-run mine-loop logs are written. */
export const RUNS_DIR = path.join(DATA_ROOT, "runs");

/**
 * The agent's OWN Sova node (JSON-RPC, e.g. http://127.0.0.1:8545), if one
 * is configured: `sova_mine` passes it as `--sova-rpc` by default so SIP-8
 * anchored burns vote for that node's head. Unset: no default (a vote is
 * only as good as the node it came from; never someone else's by default).
 */
export const DEFAULT_SOVA_RPC_URL = process.env.SOVA_NODE_RPC_URL || undefined;

/** Default regtest RPC endpoint (box/regtest/docker-compose.yml). */
export const DEFAULT_RPC_URL =
  process.env.SOVA_REGTEST_RPC_URL ?? "http://127.0.0.1:18232";
