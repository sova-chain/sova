// Reads the `sova-miner` JSON state sidecar (<data-dir>/state.json) that
// `crates/burn-wallet/miner/src/state.rs` writes. We only read it -- the CLI
// owns all writes -- to recover the funding address for `sova_fund_regtest`
// and to report structured status without re-parsing stdout.
import { readFile } from "node:fs/promises";
import path from "node:path";
export function statePath(dataDir) {
    return path.join(dataDir, "state.json");
}
export function keystorePath(dataDir) {
    return path.join(dataDir, "keystore.json");
}
/** Returns `null` if no state file exists yet (i.e. `sova_init` not run). */
export async function readMinerState(dataDir) {
    try {
        const raw = await readFile(statePath(dataDir), "utf8");
        return JSON.parse(raw);
    }
    catch (err) {
        if (err.code === "ENOENT") {
            return null;
        }
        throw err;
    }
}
//# sourceMappingURL=minerState.js.map