// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Vm} from "forge-std/Vm.sol";
import {ZcashStatus, ZCASH_PRECOMPILE} from "../../src/zcash/IZcash.sol";
import {ZcashPool} from "../../src/zcash/IZcashPools.sol";
import {MockZcash} from "./MockZcash.sol";

/// @title MockZcashPools: MockZcash plus the SIP-7 v1.1 pool methods
/// @notice Etched at ZCASH_PRECOMPILE by {installMockZcashPools}. Answers
/// IZcashPools with the same selectors and byte-identical return data (it
/// does not inherit the interface only because `blockStats` and
/// `txShielded` return static structs, whose encoding equals the flat tuples, to stay
/// under the legacy codegen's stack limit).
///
/// Semantics mirror SIP-7 §2 on top of MockZcash's segment Z[base .. anchor]:
/// - Height-keyed: OUT_OF_RANGE for h < base, NOT_YET for h > anchor, then
///   NO_SUCH_POOL for pool >= poolCount (6 unless `setPoolCount`). Heights
///   never scripted answer OK with zeros (the real index has every height).
/// - txShielded: NOT_FOUND unless the tx is visible (MockZcash rules); a
///   visible tx without scripted shielded data answers OK with zeros.
/// - `pushPools(values)` mines the next block with those totals and
///   deltas = values - previous totals, as Zebra computes them.
contract MockZcashPools is MockZcash {
    struct Stats {
        uint8 status;
        uint32 txCount;
        uint32 shieldedTxCount;
        uint32 tIn;
        uint32 tOut;
        uint32 saplingSpends;
        uint32 saplingOutputs;
        uint32 orchardActions;
        uint32 ironwoodActions;
        uint32 joinSplits;
        uint64 saplingNotes;
        uint64 orchardNotes;
        uint64 ironwoodNotes;
    }

    struct TxShielded {
        uint8 status;
        uint64 height;
        int64 sproutDelta;
        int64 saplingDelta;
        int64 orchardDelta;
        int64 ironwoodDelta;
        uint32 nIn;
        uint32 saplingSpends;
        uint32 saplingOutputs;
        uint32 orchardActions;
        uint32 ironwoodActions;
        uint32 joinSplits;
    }

    struct Shielded {
        int64[4] deltas; // sprout, sapling, orchard, ironwood
        uint32 nIn;
        uint32[5] counts; // saplingSpends, saplingOutputs, orchardActions, ironwoodActions, joinSplits
    }

    uint8 public poolCount = ZcashPool.COUNT;
    mapping(uint64 => uint64[6]) internal poolVals;
    mapping(uint64 => int64[6]) internal poolDeltas;
    mapping(uint64 => Stats) internal stats;
    mapping(bytes32 => Shielded) internal shielded;

    // ------------------------------------------------------------------
    // Scripting
    // ------------------------------------------------------------------

    function setPools(uint64 h, uint64[6] memory values, int64[6] memory deltas) public {
        poolVals[h] = values;
        poolDeltas[h] = deltas;
    }

    /// @notice Mine one block at anchor + 1 with these pool totals.
    function pushPools(uint64[6] calldata values) external returns (uint64 h) {
        h = anchorHeight + 1;
        uint64[6] storage prev = poolVals[anchorHeight];
        int64[6] memory d;
        for (uint256 i = 0; i < 6; i++) {
            d[i] = int64(int256(uint256(values[i])) - int256(uint256(prev[i])));
        }
        setPools(h, values, d);
        anchorHeight = h;
    }

    /// @dev `s.status` is ignored (the mock derives it from the height).
    function setStats(uint64 h, Stats calldata s) external {
        stats[h] = s;
    }

    function setTxShielded(bytes32 txid, int64[4] calldata deltas, uint32 nIn, uint32[5] calldata counts) external {
        require(txs[txid].exists, "MockZcashPools: no tx");
        shielded[txid] = Shielded({deltas: deltas, nIn: nIn, counts: counts});
    }

    /// @notice Pretend only ids < n are defined (NO_SUCH_POOL above).
    function setPoolCount(uint8 n) external {
        poolCount = n;
    }

    // ------------------------------------------------------------------
    // IZcashPools
    // ------------------------------------------------------------------

    function poolValue(uint64 h, uint8 pool)
        external
        view
        returns (uint8 status, uint64 chainValueZat, int64 deltaZat)
    {
        status = _heightStatus(h);
        if (status != ZcashStatus.OK) return (status, 0, 0);
        if (pool >= poolCount || pool >= 6) return (ZcashStatus.NO_SUCH_POOL, 0, 0);
        return (status, poolVals[h][pool], poolDeltas[h][pool]);
    }

    function poolTotals(uint64 h)
        external
        view
        returns (uint8 status, uint64[] memory chainValueZat, int64[] memory deltaZat)
    {
        status = _heightStatus(h);
        if (status != ZcashStatus.OK) return (status, chainValueZat, deltaZat);
        uint256 n = poolCount < 6 ? poolCount : 6;
        chainValueZat = new uint64[](n);
        deltaZat = new int64[](n);
        for (uint256 i = 0; i < n; i++) {
            chainValueZat[i] = poolVals[h][i];
            deltaZat[i] = poolDeltas[h][i];
        }
    }

    function blockStats(uint64 h) external view returns (Stats memory s) {
        uint8 st = _heightStatus(h);
        if (st != ZcashStatus.OK) {
            s.status = st;
            return s;
        }
        s = stats[h];
        s.status = ZcashStatus.OK;
    }

    /// @dev Static struct return: same bytes as the interface's 12-tuple.
    function txShielded(bytes32 txid) external view returns (TxShielded memory r) {
        Tx storage t = txs[txid];
        if (!_visible(t)) {
            r.status = ZcashStatus.NOT_FOUND;
            return r;
        }
        Shielded storage x = shielded[txid];
        r.height = t.height;
        r.sproutDelta = x.deltas[0];
        r.saplingDelta = x.deltas[1];
        r.orchardDelta = x.deltas[2];
        r.ironwoodDelta = x.deltas[3];
        r.nIn = x.nIn;
        r.saplingSpends = x.counts[0];
        r.saplingOutputs = x.counts[1];
        r.orchardActions = x.counts[2];
        r.ironwoodActions = x.counts[3];
        r.joinSplits = x.counts[4];
    }

    function _heightStatus(uint64 h) internal view returns (uint8) {
        if (h < base) return ZcashStatus.OUT_OF_RANGE;
        if (h > anchorHeight) return ZcashStatus.NOT_YET;
        return ZcashStatus.OK;
    }
}

/// @notice Etch MockZcashPools at the SIP-4 address and initialize it.
function installMockZcashPools(Vm vm, uint64 base, uint64 anchor_) returns (MockZcashPools z) {
    MockZcashPools impl = new MockZcashPools();
    vm.etch(ZCASH_PRECOMPILE, address(impl).code);
    z = MockZcashPools(ZCASH_PRECOMPILE);
    // Storage is not copied by etch: re-set the non-zero default.
    z.setPoolCount(ZcashPool.COUNT);
    z.init(base, anchor_, 1_700_000_000);
}
