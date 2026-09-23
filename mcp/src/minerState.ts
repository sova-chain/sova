// Reads the `sova-miner` JSON state sidecar (<data-dir>/state.json) that
// `crates/burn-wallet/miner/src/state.rs` writes. We only read it -- the CLI
// owns all writes -- to recover the funding address for `sova_fund_regtest`
// and to report structured status without re-parsing stdout.

import { readFile } from "node:fs/promises";
import path from "node:path";

export interface EpochRecord {
  epoch: number;
  height: number;
  burn_zat: number;
  fee_zat: number;
  change_zat: number;
  txid: string;
}

export interface MinerState {
  version: number;
  address: string;
  evm_address_hex: string;
  budget_zat: number;
  per_epoch_zat: number;
  total_burned_zat: number;
  total_fee_zat: number;
  epochs: EpochRecord[];
  utxos: Array<{ txid: string; vout: number; value_zat: number }>;
}

export function statePath(dataDir: string): string {
  return path.join(dataDir, "state.json");
}

export function keystorePath(dataDir: string): string {
  return path.join(dataDir, "keystore.json");
}

/** Returns `null` if no state file exists yet (i.e. `sova_init` not run). */
export async function readMinerState(
  dataDir: string,
): Promise<MinerState | null> {
  try {
    const raw = await readFile(statePath(dataDir), "utf8");
    return JSON.parse(raw) as MinerState;
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") {
      return null;
    }
    throw err;
  }
}
