# zebrad testnet snapshots

Every Sova node runs its own zebrad. A testnet zebrad takes hours to sync,
and for strangers that is the biggest cost of joining. Decision infra-2
**D6**: the project publishes **verifiable testnet snapshots** of zebrad's
state. Each one comes with a block height, that block's hash and the
archive's SHA-256, so a newcomer can download it instead of syncing.
Mainnet snapshots come later, and only once the trust copy below is
published and there is a "sealers should full-sync" recommendation
(`docs/design/infra-m1.md` §2, D6).

Tool: `box/testnet/snapshot.sh` (`capture`, `create`, `restore`,
`verify`). Tests: `box/testnet/test/`.

## What a snapshot is

A snapshot is a **sync shortcut, not a source of truth**. It is a tarball
of one stopped zebrad's state directory, plus a manifest naming one block
that the restored node must have. The restorer checks that block against
a source the publisher doesn't control. If the check fails, the restorer
throws the snapshot away and full-syncs.

Published files, all in one directory:

| File | Contents |
| --- | --- |
| `zebrad-testnet-<height>.tar.zst` | `state/v28/testnet/` (the RocksDB database, minus `LOCK` and the `LOG*` info logs) and `non_finalized_state/testnet/` (zebrad's backup of its last ~1,000 blocks). Paths are relative to zebrad's `state.cache_dir`. If `zstd` is missing it falls back to `.tar.gz` and says so. Published snapshots should use zstd. |
| `SHA256SUMS` | `<sha256>  <archive name>`, in the format `sha256sum -c` reads. |
| `snapshot.json` | `network`, `zebra_version`, `state_version`, `height`, `hash`, `created_at`, `sha256`, `size`, plus `archive`, `compression`, `state_bytes_approx`, `contents` and `offline_check`. |

**Never in the archive:** the RPC cookie (`.cookie`, which is an auth
secret), the peer cache (`network/testnet.peers`, which holds our peers'
IP addresses), RocksDB `LOG` files (which hold host paths), `LOCK`,
other networks, and other state versions. `create` lists everything it
leaves out, and the tests check that none of it gets into the archive.

**Why the non-finalized backup is included.** zebrad 6.3.0 keeps the last
`MAX_BLOCK_REORG_HEIGHT` = **1,000** blocks outside RocksDB, in memory,
and backs them up to `non_finalized_state/<network>/` (one file per
block, named by its hash). On the live testnet node there are 1,010 such
files. Without them, the restored tip would be about 1,000 blocks (about
21 hours) behind the captured tip, and the manifest's block would not
exist yet. On start, zebrad re-validates these blocks (see "Trust model").

## Sizes (measured 2026-09-22)

| | |
| --- | --- |
| Live testnet state, height 4,382,331 | **12 GB** on disk (`du -sh`: `state/` 12 GB, `non_finalized_state/` 19 MB). `getblockchaininfo.size_on_disk` = 12,428,527,544 bytes. |
| Archive | Not measured yet. Expect it to be close to the state size [est], because zebra already LZ4-compresses its SST files, so zstd -3 on top gains little. The first real run records the exact size in `snapshot.json`. |
| Disk needed to create | About 1× the state for the archive, on the output volume. `create` refuses if there isn't room. |
| Disk needed to restore | Archive plus state (about 2× while both exist). `restore` checks free space against `state_bytes_approx`. |

infra-m1.md estimates about 25–40 GB at tip. The measured 12 GB is
smaller, so plan R2 storage on the measured number and re-measure at each
snapshot.

## Operator runbook (publishing a snapshot)

The database **must be stopped**. RocksDB's files are not consistent
while zebrad has the database open, and zebra 6.3.0 has no online-backup
or RocksDB-checkpoint command. (`zebrad copy-state` is a debug tool that
re-commits blocks into a new database. `zebrad tip-height` only prints
the finalized height.) So the height and hash are captured over RPC
while the node runs. Then the node is stopped, and the archive is made
from the stopped directory.

```bash
SNAP=box/testnet/snapshot.sh
STATE="/path/to/zebra-testnet-state"   # zebrad's state.cache_dir
RPC=http://127.0.0.1:18234                               # that zebrad's RPC

# 1. Capture, while the node runs: network (by genesis hash), tip height,
#    tip hash, zebra version. Add --rpc-cookie-file <cache_dir>/.cookie if
#    cookie auth is on.
$SNAP capture --rpc $RPC --out /tmp/capture.json

# 2. Wait ~10 s (the non-finalized backup is written at most every 5 s),
#    then stop zebrad gracefully (SIGINT/SIGTERM, or systemctl stop). Wait
#    for the process to exit. zebrad flushes RocksDB on a clean shutdown.

# 3. Create. It refuses if any process still holds the database's RocksDB
#    lock, if any process has LOCK open, if a running Docker container
#    mounts the directory, or if --rpc still answers.
$SNAP create "$STATE" "/path/to/snapshots/next" \
  --capture /tmp/capture.json --rpc $RPC

# 4. Restart zebrad. Downtime is the archive time.
```

To cut downtime on APFS, stop zebrad, make a clone with
`cp -cR "$STATE" "$STATE.snap"` (copy-on-write, near-instant), restart
zebrad, then run `create` on the clone and delete the clone afterwards.

5. **Test-restore before publishing.** Restore the archive into a
   scratch directory. Start a second zebrad on it with its own ports,
   then check it against the live node:

   ```bash
   $SNAP restore <out>/zebrad-testnet-<h>.tar.zst /path/to/scratch
   # start the scratch zebrad (cache_dir=/path/to/scratch, other RPC/P2P ports)
   $SNAP verify --rpc http://127.0.0.1:<scratch-rpc> --manifest <out>/snapshot.json \
     --reference-rpc $RPC
   ```

6. **Publish** (later, not part of this kit): upload the archive,
   `SHA256SUMS` and `snapshot.json` to R2 (`dl.<domain>`). Also post the
   height, hash and SHA-256 **somewhere other than the bucket**: a signed
   commit to this repo's docs, the release notes, or the project's
   channels. If the only copy of the checksum sits next to the file, it
   only proves the download wasn't corrupted in transit. It doesn't
   prove who published the file.

`create` warns (and records the warning in `offline_check`) if the
captured block is not in the non-finalized backup. That means the capture
was stale, or the node was stopped less than 5 s after that block
arrived. The test restore settles it.

## Newcomer runbook (using a snapshot)

```bash
# 1. Download all three files into one directory.
# 2. Check the checksum and restore into a NEW, empty directory:
box/testnet/snapshot.sh restore zebrad-testnet-<h>.tar.zst ~/sova/zebra-state
#    It refuses a bad checksum, a missing SHA256SUMS, a snapshot.json that
#    belongs to another archive, a non-empty target, and archives that hold
#    anything other than state/ and non_finalized_state/ (or any links).
# 3. In zebrad.toml: [state] cache_dir = "~/sova/zebra-state" (absolute path),
#    network = "Testnet". Use the zebrad version from snapshot.json
#    (zfnd/zebra:6.3.0, state format 28). An older zebrad ignores the v28
#    directory and full-syncs. A newer one reuses it only if its format can
#    be restored from v28, and otherwise deletes it at startup.
# 4. Start zebrad. Then:
box/testnet/snapshot.sh verify --rpc http://127.0.0.1:<rpc-port> --manifest snapshot.json
# 5. Compare the manifest's hash at that height with an INDEPENDENT source:
#    any public Zcash testnet block explorer, or `getblockhash <height>` on a
#    second node you run. Also check that your tip keeps advancing and
#    matches the explorer's tip.
```

If the hash differs, or the node stalls and won't follow the network,
stop zebrad, delete the directory and full-sync.

## Trust model

**What the checksum proves.** `SHA256SUMS` proves the file you have is
the file the publisher hashed. It protects against corruption and
mirrors. It proves nothing about the publisher, because it comes from
the same place as the archive. The independent block-hash comparison
(runbook step 5) is the check that counts.

**What zebrad re-checks when it starts on an imported state** (zebra
6.3.0 source, read for this doc):

1. **Format validity checks**
   (`disk_format/upgrade.rs: format_validity_checks_detailed`): the
   database's structure (column families, tree key types, subtrees,
   cached genesis roots). This is about structure, not consensus.
2. **State checkpoint validation** (`zebra-consensus/src/router.rs`).
   For every hard-coded checkpoint height up to the state's tip, the
   best-chain block hash at that height must equal the compiled-in
   checkpoint hash. If one doesn't, zebrad panics ("invalid block in
   state … Delete and re-sync"). Testnet checkpoints in 6.3.0 run every
   few hundred blocks, up to height **4,238,000**. This pins the chain's
   block hashes at those heights.
3. **Non-finalized backup blocks** (the last ~1,000) are recommitted
   through `validate_and_commit_non_finalized`. That runs contextual
   checks against the finalized state: difficulty and time rules,
   nullifier and UTXO consistency, note-commitment anchors and tree
   roots. It does **not** re-run proofs, signatures or scripts. Blocks
   that fail are dropped with a warning.
4. **New blocks from peers** are fully verified (semantic and
   contextual) on top of the imported state. The testnet tip is above
   the last checkpoint, so nothing new is checkpoint-trusted.

**What zebrad does NOT re-check.** Everything else in the finalized
database is trusted as it stands: the stored transactions, the block
hashes between checkpoint heights and above 4,238,000, the UTXO set,
the nullifier sets, the note-commitment trees and the value pools.
That is the risk a doctored snapshot poses. It could, for example,
insert or drop a Zcash burn.

**Why that's acceptable for Sova testnet.** The damage stays local and
shows up loudly (infra-m1.md §2):

- A doctored tip can't connect to the real chain. The node stops
  following peers, and the explorer comparison catches it immediately.
- For each Zcash height from the Sova base onward, the Sova chain
  commits to the burns: settlements are in the block withdrawals. A
  snapshot that adds or drops a burn makes the restoring node's C5
  check reject a canonical Sova block. The node **stalls loudly**
  instead of diverging silently. (This holds modulo the accept-unknown
  debt noted in the C3 row.)
- Doctored UTXO data could mislead the node's own wallet view. The
  network still rejects spends that don't exist.
- A bad snapshot can harm only the node that restored it, never other
  nodes.

Anyone who doesn't accept this should full-sync. That remains the
recommendation for sealers on mainnet.

## Tests

```bash
box/testnet/test/snapshot-refusals.sh        # ~2 s, no Docker
box/testnet/test/snapshot-e2e-regtest.sh     # regtest round trip in Docker (150 blocks, ~1 min)
box/testnet/test/snapshot-e2e-regtest.sh 1100  # also fills the finalized RocksDB (~4.5 min)
```

`snapshot-refusals.sh` covers create's refusals (database locked by a
process, `LOCK` held open, RPC still answering, missing or bad
height/hash, non-empty output, capture/state network mismatch, mainnet,
not a cache_dir) and restore's refusals (non-empty target, tampered
archive, missing `SHA256SUMS`, mismatched manifest, unexpected archive
entries). It also covers the happy paths and the gzip fallback.

The e2e test uses its own Compose project (`sova-snap`), container
(`sova-zebrad-snap`) and port (`127.0.0.1:18352`), with state in a temp
dir on the internal disk. It generates blocks on node A, captures, and
checks that `create` is refused while A runs. It then stops A, creates,
restores into a fresh dir, and starts node B on the restored state. It
passes only if B's tip height and hash equal the captured ones and
`verify` passes. Evidence from 2026-09-22, 1,100 blocks: tip `1100
e6f6dd82…7086f` on both nodes, archive 233,597 bytes, sha256
`b1c3f0c7…e730`. On B, 100 blocks came from RocksDB and 1,000 were
restored from the backup (`num_blocks_restored=1000`).

**Docker Desktop note.** The macOS virtualization process keeps
bind-mounted files open even after a container exits. `create`
therefore ignores that process in its `lsof` check. It relies on the
running-container mount check instead, because a zebrad inside the VM
holds its lock in the VM's kernel, where the host's `fcntl` probe can't
see it.
