// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IZcash, ZcashStatus, ZCASH_PRECOMPILE} from "./IZcash.sol";
import {IZcashPools, ZcashPool} from "./IZcashPools.sol";

/// @title ZcashLib: checks against the SIP-4 precompile
/// @notice "Did this transparent output pay at least X to script S, at
/// depth >= minConf?" plus P2PKH / P2SH script builders.
///
/// `minConf` is REQUIRED on every check and has no default; 0 is rejected.
/// Recommended: testnet 3, mainnet 10 (about 12.5 min at 75 s per block),
/// more for large values. A Zcash reorg below the payment's depth removes
/// the payment and SIP-4 reorgs Sova with it, so on-Sova state stays
/// consistent; minConf protects whatever happens *outside* Sova meanwhile.
///
/// Caveats:
/// - Pre-v5 (v4) txids are malleable until mined: a third party can
///   re-encode the signatures and change the txid. Key on what an output
///   pays (script and value), or reserve before payment, rather than
///   trusting a txid chosen in advance. Only one version can be mined.
/// - Coinbase transactions are included. Their outputs cannot be spent
///   transparently on testnet/mainnet until shielded.
/// - Only transparent outputs are visible. Shielded amounts, recipients
///   and memos are not, by Zcash's design. A shielded wallet can still
///   pay a t-address (z->t), and that output is visible.
///
/// Pool helpers (SIP-7, see IZcashPools): read the six Zcash value pools
/// and their per-block changes. Sign convention: pool delta, + = value
/// INTO the pool (= Zebra `valueDeltaZat` = -valueBalance for a tx).
/// "Shielded" means Sprout + Sapling + Orchard + Ironwood (not transparent,
/// not the lockbox). Each helper has a form that reads the precompile and a
/// form that takes the source explicitly, so the same code can read the
/// ZcashBlocks ring (`0x…5A01`, height-keyed methods only, last 8,191
/// anchored heights) instead. Pool helpers return the raw ZcashStatus
/// (NO_SUCH_POOL = 6 included) rather than {Result}.
library ZcashLib {
    IZcash internal constant ZCASH = IZcash(ZCASH_PRECOMPILE);
    /// @dev The SIP-7 pool methods live on the same precompile.
    IZcashPools internal constant POOLS = IZcashPools(ZCASH_PRECOMPILE);

    /// @notice Outcome of a non-reverting check. Values 1..4 mirror the
    /// precompile status codes they come from.
    enum Result {
        OK,
        NOT_FOUND,
        NOT_YET,
        OUT_OF_RANGE,
        NO_SUCH_OUTPUT,
        INSUFFICIENT_CONFIRMATIONS,
        WRONG_SCRIPT,
        UNDERPAID,
        BAD_STATUS
    }

    /// @notice What a checked payment looked like (zeroes when unknown).
    struct Payment {
        uint64 height;
        uint64 confirmations;
        uint64 valueZat;
    }

    error ZcashMinConfZero();
    error ZcashTxNotFound(bytes32 txid);
    error ZcashNotYet(bytes32 txid);
    error ZcashOutOfRange(bytes32 txid);
    error ZcashNoSuchOutput(bytes32 txid, uint32 vout);
    error ZcashInsufficientConfirmations(bytes32 txid, uint64 confirmations, uint64 minConf);
    error ZcashWrongScript(bytes32 txid, uint32 vout);
    error ZcashUnderpaid(bytes32 txid, uint32 vout, uint64 valueZat, uint64 minZat);
    error ZcashBadStatus(uint8 status);
    /// @notice A pool read at height `height` returned non-OK `status`.
    error ZcashPoolRead(uint64 height, uint8 status);
    /// @notice The precompile call failed or returned the wrong length
    /// (no SIP-7 precompile at 0x…5A00).
    error ZcashPoolCall();

    // ------------------------------------------------------------------
    // Checks
    // ------------------------------------------------------------------

    /// @notice Non-reverting check that output (txid, vout) pays at least
    /// `minZat` to exactly `script`, mined at depth >= `minConf`.
    /// @dev Reverts only if `minConf == 0` or the precompile call itself
    /// fails (not deployed: empty returndata; malformed call).
    /// Cost: txInfo (4,000) + txOutput (4,000 + 8/script byte) when the
    /// depth check passes; txInfo only otherwise.
    function outputPays(bytes32 txid, uint32 vout, bytes memory script, uint64 minZat, uint64 minConf)
        internal
        view
        returns (Result result, Payment memory p)
    {
        if (minConf == 0) revert ZcashMinConfZero();

        (uint8 st, uint64 height,, uint64 conf,,) = ZCASH.txInfo(txid);
        if (st != ZcashStatus.OK) return (_fromStatus(st), p);
        p.height = height;
        p.confirmations = conf;
        if (conf < minConf) return (Result.INSUFFICIENT_CONFIRMATIONS, p);

        (uint8 st2, uint64 valueZat, bytes memory outScript) = ZCASH.txOutput(txid, vout);
        if (st2 != ZcashStatus.OK) return (_fromStatus(st2), p);
        p.valueZat = valueZat;
        if (keccak256(outScript) != keccak256(script)) return (Result.WRONG_SCRIPT, p);
        if (valueZat < minZat) return (Result.UNDERPAID, p);
        return (Result.OK, p);
    }

    /// @notice Reverting form of {outputPays}. Each failure is its own
    /// custom error; NOT_FOUND, NOT_YET and OUT_OF_RANGE stay distinct.
    function requireOutputPays(bytes32 txid, uint32 vout, bytes memory script, uint64 minZat, uint64 minConf)
        internal
        view
        returns (Payment memory p)
    {
        Result r;
        (r, p) = outputPays(txid, vout, script, minZat, minConf);
        if (r == Result.OK) return p;
        if (r == Result.NOT_FOUND) revert ZcashTxNotFound(txid);
        if (r == Result.NOT_YET) revert ZcashNotYet(txid);
        if (r == Result.OUT_OF_RANGE) revert ZcashOutOfRange(txid);
        if (r == Result.NO_SUCH_OUTPUT) revert ZcashNoSuchOutput(txid, vout);
        if (r == Result.INSUFFICIENT_CONFIRMATIONS) {
            revert ZcashInsufficientConfirmations(txid, p.confirmations, minConf);
        }
        if (r == Result.WRONG_SCRIPT) revert ZcashWrongScript(txid, vout);
        if (r == Result.UNDERPAID) revert ZcashUnderpaid(txid, vout, p.valueZat, minZat);
        revert ZcashBadStatus(_lastBadStatus(txid, vout));
    }

    /// @notice Current anchor height E_N (the Zcash clock for this block).
    function anchorHeight() internal view returns (uint64 h) {
        (h,) = ZCASH.anchor();
    }

    /// @notice True iff a SIP-4 precompile answers at ZCASH_PRECOMPILE.
    /// Use it to fail fast on chains without SIP-4 (anvil, old Sova).
    function available() internal view returns (bool) {
        (bool ok, bytes memory ret) = ZCASH_PRECOMPILE.staticcall(abi.encodeCall(IZcash.anchor, ()));
        return ok && ret.length == 64;
    }

    // ------------------------------------------------------------------
    // Script builders
    // ------------------------------------------------------------------

    /// @notice P2PKH scriptPubKey (t1... mainnet, tm... testnet):
    /// OP_DUP OP_HASH160 <20> OP_EQUALVERIFY OP_CHECKSIG (25 bytes).
    function p2pkh(bytes20 pubKeyHash) internal pure returns (bytes memory) {
        return abi.encodePacked(hex"76a914", pubKeyHash, hex"88ac");
    }

    /// @notice P2SH scriptPubKey (t3... mainnet, t2... testnet):
    /// OP_HASH160 <20> OP_EQUAL (23 bytes).
    function p2sh(bytes20 scriptHash) internal pure returns (bytes memory) {
        return abi.encodePacked(hex"a914", scriptHash, hex"87");
    }

    // ------------------------------------------------------------------
    // Pool state (SIP-7)
    // ------------------------------------------------------------------

    /// @notice Reverting read of one pool's total after block `h`.
    /// @dev Reverts {ZcashPoolRead} on any non-OK status. 2,600 gas on the
    /// precompile.
    function requirePoolValue(uint64 h, uint8 pool) internal view returns (uint64) {
        return requirePoolValue(POOLS, h, pool);
    }

    function requirePoolValue(IZcashPools src, uint64 h, uint8 pool) internal view returns (uint64 valueZat) {
        uint8 st;
        (st, valueZat,) = src.poolValue(h, pool);
        if (st != ZcashStatus.OK) revert ZcashPoolRead(h, st);
    }

    /// @notice The change block `h` made to `pool` (+ = into the pool).
    function poolDelta(uint64 h, uint8 pool) internal view returns (uint8 status, int64 deltaZat) {
        return poolDelta(POOLS, h, pool);
    }

    function poolDelta(IZcashPools src, uint64 h, uint8 pool) internal view returns (uint8 status, int64 deltaZat) {
        (status,, deltaZat) = src.poolValue(h, pool);
    }

    /// @notice Sprout + Sapling + Orchard + Ironwood after block `h`, in
    /// one poolTotals call (2,600 gas on the precompile).
    function shieldedTotal(uint64 h) internal view returns (uint8 status, uint64 totalZat) {
        return shieldedTotal(POOLS, h);
    }

    function shieldedTotal(IZcashPools src, uint64 h) internal view returns (uint8 status, uint64 totalZat) {
        (uint8 st, uint64[] memory v,) = src.poolTotals(h);
        if (st != ZcashStatus.OK) return (st, 0);
        return (st, v[ZcashPool.SPROUT] + v[ZcashPool.SAPLING] + v[ZcashPool.ORCHARD] + v[ZcashPool.IRONWOOD]);
    }

    /// @notice Net change block `h` made to the shielded pools combined
    /// (+ = value was shielded). Orchard->Ironwood moves cancel out.
    function shieldedDelta(uint64 h) internal view returns (uint8 status, int64 deltaZat) {
        return shieldedDelta(POOLS, h);
    }

    function shieldedDelta(IZcashPools src, uint64 h) internal view returns (uint8 status, int64 deltaZat) {
        (uint8 st,, int64[] memory d) = src.poolTotals(h);
        if (st != ZcashStatus.OK) return (st, 0);
        return (st, d[ZcashPool.SPROUT] + d[ZcashPool.SAPLING] + d[ZcashPool.ORCHARD] + d[ZcashPool.IRONWOOD]);
    }

    /// @notice How much `pool` changed from after block `fromH` to after
    /// block `toH`: value(toH) - value(fromH). Two O(1) reads, whatever the
    /// window ("last day" is `toH - 1152` at 75 s blocks).
    /// @return status OK, or the first non-OK status of the two reads.
    function poolChange(uint64 fromH, uint64 toH, uint8 pool) internal view returns (uint8 status, int256 changeZat) {
        return poolChange(POOLS, fromH, toH, pool);
    }

    function poolChange(IZcashPools src, uint64 fromH, uint64 toH, uint8 pool)
        internal
        view
        returns (uint8 status, int256 changeZat)
    {
        (uint8 s0, uint64 a,) = src.poolValue(fromH, pool);
        if (s0 != ZcashStatus.OK) return (s0, 0);
        (uint8 s1, uint64 b,) = src.poolValue(toH, pool);
        if (s1 != ZcashStatus.OK) return (s1, 0);
        return (ZcashStatus.OK, int256(uint256(b)) - int256(uint256(a)));
    }

    /// @notice shieldedTotal(toH) - shieldedTotal(fromH).
    function shieldedChange(uint64 fromH, uint64 toH) internal view returns (uint8 status, int256 changeZat) {
        return shieldedChange(POOLS, fromH, toH);
    }

    function shieldedChange(IZcashPools src, uint64 fromH, uint64 toH)
        internal
        view
        returns (uint8 status, int256 changeZat)
    {
        (uint8 s0, uint64 a) = shieldedTotal(src, fromH);
        if (s0 != ZcashStatus.OK) return (s0, 0);
        (uint8 s1, uint64 b) = shieldedTotal(src, toH);
        if (s1 != ZcashStatus.OK) return (s1, 0);
        return (ZcashStatus.OK, int256(uint256(b)) - int256(uint256(a)));
    }

    /// @notice True iff `pool` first reached `threshold` at block `h`:
    /// value(h) >= threshold and value(h-1) < threshold. This is the O(1)
    /// witness check a keeper-supplied height needs (SIP-7 §4.3). False on
    /// any non-OK read (h-1 below the source's range included).
    function crossedAbove(IZcashPools src, uint64 h, uint8 pool, uint64 threshold) internal view returns (bool) {
        if (h == 0) return false;
        (uint8 s1, uint64 now_,) = src.poolValue(h, pool);
        if (s1 != ZcashStatus.OK || now_ < threshold) return false;
        (uint8 s0, uint64 before,) = src.poolValue(h - 1, pool);
        return s0 == ZcashStatus.OK && before < threshold;
    }

    /// @notice True iff `pool` first fell below `threshold` at block `h`.
    function crossedBelow(IZcashPools src, uint64 h, uint8 pool, uint64 threshold) internal view returns (bool) {
        if (h == 0) return false;
        (uint8 s1, uint64 now_,) = src.poolValue(h, pool);
        if (s1 != ZcashStatus.OK || now_ >= threshold) return false;
        (uint8 s0, uint64 before,) = src.poolValue(h - 1, pool);
        return s0 == ZcashStatus.OK && before >= threshold;
    }

    /// @notice {crossedAbove} for the shielded total.
    function shieldedCrossedAbove(IZcashPools src, uint64 h, uint64 threshold) internal view returns (bool) {
        if (h == 0) return false;
        (uint8 s1, uint64 now_) = shieldedTotal(src, h);
        if (s1 != ZcashStatus.OK || now_ < threshold) return false;
        (uint8 s0, uint64 before) = shieldedTotal(src, h - 1);
        return s0 == ZcashStatus.OK && before < threshold;
    }

    /// @notice Net value that LEFT the shielded pools in tx `txid`:
    /// -(sprout + sapling + orchard + ironwood deltas). Positive for a
    /// deshield (z->t), negative for a shield (t->z), about zero for a
    /// pure Orchard->Ironwood migration. Includes the fee. Use it for "was
    /// this payment funded from a shielded balance?" (SIP-7 §5 use 3): a
    /// txOutput payment of X whose tx has shieldedOutflow >= X was.
    /// @return status OK or NOT_FOUND.
    function shieldedOutflow(bytes32 txid) internal view returns (uint8 status, uint64 height, int256 outflowZat) {
        // Low-level call and a prefix decode: destructuring the 12-value
        // tuple is too deep for the legacy code generator. abi.decode still
        // validates each word it reads.
        (bool ok, bytes memory ret) = address(POOLS).staticcall(abi.encodeCall(IZcashPools.txShielded, (txid)));
        if (!ok || ret.length != 12 * 32) revert ZcashPoolCall();
        int64[4] memory d;
        (status, height, d[0], d[1], d[2], d[3]) = abi.decode(ret, (uint8, uint64, int64, int64, int64, int64));
        if (status != ZcashStatus.OK) return (status, 0, 0);
        outflowZat = -(int256(d[0]) + int256(d[1]) + int256(d[2]) + int256(d[3]));
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    function _fromStatus(uint8 st) private pure returns (Result) {
        if (st == ZcashStatus.NOT_FOUND) return Result.NOT_FOUND;
        if (st == ZcashStatus.NOT_YET) return Result.NOT_YET;
        if (st == ZcashStatus.OUT_OF_RANGE) return Result.OUT_OF_RANGE;
        if (st == ZcashStatus.NO_SUCH_OUTPUT) return Result.NO_SUCH_OUTPUT;
        return Result.BAD_STATUS;
    }

    /// @dev Cold path only: re-reads which status was unknown, for the error.
    function _lastBadStatus(bytes32 txid, uint32 vout) private view returns (uint8) {
        (uint8 st,,,,,) = ZCASH.txInfo(txid);
        if (st != ZcashStatus.OK) return st;
        (uint8 st2,,) = ZCASH.txOutput(txid, vout);
        return st2;
    }
}
