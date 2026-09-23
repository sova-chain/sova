// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title Zcash value-pool ids (SIP-7 §2)
/// @notice Zebra's `valuePools` order. A future Zcash pool is appended as
/// id 6 at a Sova fork height, so ids never change meaning and the
/// `poolTotals` arrays grow instead of changing shape.
library ZcashPool {
    uint8 internal constant TRANSPARENT = 0;
    uint8 internal constant SPROUT = 1;
    uint8 internal constant SAPLING = 2;
    uint8 internal constant ORCHARD = 3;
    /// @dev The NU6 dev-fund lockbox. Not a shielded pool.
    uint8 internal constant LOCKBOX = 4;
    /// @dev NU6.3. Shielded coinbase lands here from NU6.3 on.
    uint8 internal constant IRONWOOD = 5;
    /// @dev Pools defined in SIP-7 v1.1 (ids 0..5).
    uint8 internal constant COUNT = 6;

    /// @notice Sprout, Sapling, Orchard and Ironwood. Transparent and the
    /// lockbox are not shielded.
    function isShielded(uint8 pool) internal pure returns (bool) {
        return pool == SPROUT || pool == SAPLING || pool == ORCHARD || pool == IRONWOOD;
    }
}

/// @title IZcashPools: SIP-7 v1.1 pool-state queries on the SIP-4 precompile
/// @notice Same address as IZcash (`0x…5A00`), same Solidity-ABI dispatch,
/// same read-only rules and the same coverage-then-answer rule (SIP-4 §3,
/// §5). Every answer is a pure function of the Zcash chain up to the
/// queried block, identical on every honest node. See SIP-7 §2.
///
/// Sign convention: POOL DELTA, positive = value INTO the pool, everywhere
/// (blocks and transactions alike). It equals Zebra's per-block
/// `valuePools[i].valueDeltaZat`, and for a transaction it is
/// `-valueBalance` of that pool's bundle (Zcash's `valueBalance > 0` means
/// value LEAVES the pool). For Sprout it is `sum(vpub_old - vpub_new)`.
///
/// Statuses (ZcashStatus in IZcash.sol): OK = 0; NOT_FOUND = 1 (txShielded:
/// tx not in Z[B .. E_N]); NOT_YET = 2 (h > E_N); OUT_OF_RANGE = 3 (h < B);
/// NO_SUCH_POOL = 6 (poolValue: pool id not defined at height h). A non-OK
/// status zeroes every other field (poolTotals: empty arrays). Statuses are
/// results, never reverts; malformed calldata (including a non
/// sign-extended int64 word or a uint8 word above 255) reverts.
///
/// Gas (SIP-7 §2): poolValue, poolTotals, blockStats 2,600; txShielded
/// 4,000. Negative answers cost the same as positive ones.
///
/// Privacy (SIP-7 §4.4): these are aggregates and public counts. Amounts,
/// senders, recipients and memos of shielded transfers stay private.
/// Caveats: action/spend/output counts are an upper bound on payments
/// (Orchard and Ironwood pad to >= 2 actions, wallets add dummy Sapling
/// outputs); a tx's net shielded flow includes its fee (the fee alone is not
/// available).
interface IZcashPools {
    /// @notice Total value in `pool` after Zcash block `h`, and the change
    /// block `h` made to it.
    /// @param pool A {ZcashPool} id.
    /// @return status OK, NOT_YET, OUT_OF_RANGE or NO_SUCH_POOL.
    /// @return chainValueZat The pool's total (Zebra `chainValueZat`). A
    /// level, not a counter: it can go up or down.
    /// @return deltaZat chainValue(h) - chainValue(h-1) (Zebra
    /// `valueDeltaZat`); + means value entered the pool.
    function poolValue(uint64 h, uint8 pool)
        external
        view
        returns (uint8 status, uint64 chainValueZat, int64 deltaZat);

    /// @notice Every pool at once, indexed by {ZcashPool} id. Length 6 in
    /// v1.1; later pools append.
    /// @return status OK, NOT_YET or OUT_OF_RANGE (arrays empty unless OK).
    function poolTotals(uint64 h)
        external
        view
        returns (uint8 status, uint64[] memory chainValueZat, int64[] memory deltaZat);

    /// @notice Public activity counts for Zcash block `h`.
    /// @return status OK, NOT_YET or OUT_OF_RANGE.
    /// @return txCount Transactions in the block, coinbase included.
    /// @return shieldedTxCount Transactions with any Sprout, Sapling, Orchard
    /// or Ironwood component, coinbase included.
    /// @return tIn Transparent inputs (vin entries) in the block.
    /// @return tOut Transparent outputs (vout entries) in the block.
    /// @return saplingSpends Sapling spends (one nullifier each).
    /// @return saplingOutputs Sapling outputs (one note commitment each).
    /// @return orchardActions Orchard actions (one nullifier and one note
    /// commitment each).
    /// @return ironwoodActions Ironwood actions (likewise).
    /// @return joinSplits Sprout JoinSplit descriptions.
    /// @return saplingNotes Cumulative Sapling note-commitment tree size
    /// after block `h` (every Sapling note ever created). Per-block counts
    /// are differences of consecutive heights.
    /// @return orchardNotes Cumulative Orchard tree size after `h`.
    /// @return ironwoodNotes Cumulative Ironwood tree size after `h` (0
    /// before its first note).
    function blockStats(uint64 h)
        external
        view
        returns (
            uint8 status,
            uint32 txCount,
            uint32 shieldedTxCount,
            uint32 tIn,
            uint32 tOut,
            uint32 saplingSpends,
            uint32 saplingOutputs,
            uint32 orchardActions,
            uint32 ironwoodActions,
            uint32 joinSplits,
            uint64 saplingNotes,
            uint64 orchardNotes,
            uint64 ironwoodNotes
        );

    /// @notice The shielded side of a mined transaction. Works for any tx
    /// in Z[B .. E_N]; a tx with no shielded component returns OK with zero
    /// deltas and counts.
    /// @return status OK or NOT_FOUND.
    /// @return height Zcash height of the containing block.
    /// @return sproutDelta Pool delta for Sprout (+ = into the pool).
    /// @return saplingDelta Pool delta for Sapling (= -valueBalanceZat).
    /// @return orchardDelta Pool delta for Orchard (= -orchard.valueBalanceZat;
    /// <= 0 from NU6.3 on, since Orchard can only shrink).
    /// @return ironwoodDelta Pool delta for Ironwood (= -ironwood.valueBalanceZat).
    /// @return nIn Transparent input count ("fully shielded" is
    /// `nIn == 0 && nOut == 0`, with nOut from IZcash.txInfo).
    /// @return saplingSpends Sapling spends in this tx.
    /// @return saplingOutputs Sapling outputs in this tx.
    /// @return orchardActions Orchard actions in this tx.
    /// @return ironwoodActions Ironwood actions in this tx.
    /// @return joinSplits Sprout JoinSplits in this tx.
    /// @dev Net value that LEFT the shielded pools in this tx is
    /// -(sproutDelta + saplingDelta + orchardDelta + ironwoodDelta). It
    /// includes the fee, and nets to about zero for an Orchard->Ironwood
    /// migration.
    function txShielded(bytes32 txid)
        external
        view
        returns (
            uint8 status,
            uint64 height,
            int64 sproutDelta,
            int64 saplingDelta,
            int64 orchardDelta,
            int64 ironwoodDelta,
            uint32 nIn,
            uint32 saplingSpends,
            uint32 saplingOutputs,
            uint32 orchardActions,
            uint32 ironwoodActions,
            uint32 joinSplits
        );
}
