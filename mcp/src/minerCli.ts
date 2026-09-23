// Thin wrapper around invoking the `sova-miner` binary as a one-shot child
// process (`init`, `report`) -- see runManager.ts for the long-running
// `mine` loop, which needs different (backgrounded, tailable) handling.

import { execFile } from "node:child_process";
import { access, constants as fsConstants } from "node:fs/promises";
import { BURN_WALLET_DIR, MINER_BIN } from "./paths.js";

export interface CliResult {
  command: string;
  exitCode: number | null;
  stdout: string;
  stderr: string;
}

export class MinerBinaryMissingError extends Error {
  constructor() {
    super(
      `sova-miner binary not found at ${MINER_BIN}. Build it first:\n` +
        `  cd ${BURN_WALLET_DIR} && cargo build --release -p sova-miner\n` +
        `(crates/burn-wallet is a nested cargo workspace -- see its Cargo.toml header comment.)`,
    );
    this.name = "MinerBinaryMissingError";
  }
}

export async function assertMinerBinaryExists(): Promise<void> {
  try {
    await access(MINER_BIN, fsConstants.X_OK);
  } catch {
    throw new MinerBinaryMissingError();
  }
}

/**
 * Runs `sova-miner <args>` to completion and captures its output. Does NOT
 * throw on a non-zero exit code -- callers (tool handlers) decide how to
 * surface a CLI-reported failure (e.g. `report --verify-rpc` mismatch)
 * versus a real invocation error.
 */
export async function runMinerOnce(
  args: string[],
  timeoutMs = 60_000,
): Promise<CliResult> {
  await assertMinerBinaryExists();

  return new Promise<CliResult>((resolve, reject) => {
    execFile(
      MINER_BIN,
      args,
      { timeout: timeoutMs, maxBuffer: 16 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error && typeof error.code !== "number" && error.signal) {
          // Killed by timeout/signal rather than a normal non-zero exit.
          reject(
            new Error(
              `sova-miner ${args.join(" ")} was killed (${error.signal}) after ${timeoutMs}ms:\n${stderr}`,
            ),
          );
          return;
        }
        const exitCode =
          error && typeof error.code === "number" ? error.code : 0;
        resolve({
          command: `sova-miner ${args.join(" ")}`,
          exitCode,
          stdout,
          stderr,
        });
      },
    );
  });
}
