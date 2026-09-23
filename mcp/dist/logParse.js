// Parses the println!-based stdout of `sova-miner mine`
// (crates/burn-wallet/miner/src/mine.rs) into a short structured summary,
// so `sova_status` doesn't force the caller to eyeball a raw log tail to
// know how many epochs have landed.
const EPOCH_LINE = /^epoch (\d+): height=(\d+) burn=(\d+)zat fee=(\d+)zat change=(\d+)zat txid=(\S+)$/;
const BUDGET_EXHAUSTED_LINE = /^budget exhausted at height (\d+):/;
const MAX_EPOCHS_LINE = /^reached --max-epochs (\d+) -- stopping\.$/;
export function summarizeMineLog(logContent) {
    let epochsCompleted = 0;
    let lastEpoch = null;
    let stoppedReason = null;
    const warnings = [];
    for (const line of logContent.split("\n")) {
        const epochMatch = EPOCH_LINE.exec(line);
        if (epochMatch) {
            epochsCompleted += 1;
            lastEpoch = {
                epoch: Number(epochMatch[1]),
                height: Number(epochMatch[2]),
                burnZat: Number(epochMatch[3]),
                feeZat: Number(epochMatch[4]),
                changeZat: Number(epochMatch[5]),
                txid: epochMatch[6],
            };
            continue;
        }
        if (BUDGET_EXHAUSTED_LINE.test(line)) {
            stoppedReason = "budget-exhausted";
            continue;
        }
        if (MAX_EPOCHS_LINE.test(line)) {
            stoppedReason = "max-epochs-reached";
            continue;
        }
        if (line.startsWith("warning:") || line.startsWith("error:")) {
            warnings.push(line);
        }
    }
    return { epochsCompleted, lastEpoch, stoppedReason, warnings };
}
//# sourceMappingURL=logParse.js.map