// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IZcash, ZcashStatus, ZCASH_PRECOMPILE} from "./IZcash.sol";

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
library ZcashLib {
    IZcash internal constant ZCASH = IZcash(ZCASH_PRECOMPILE);

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
