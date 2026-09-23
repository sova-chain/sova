// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Vm} from "forge-std/Vm.sol";
import {IZcash, ZcashStatus, ZCASH_PRECOMPILE} from "../../src/zcash/IZcash.sol";

/// @title MockZcash: scriptable stand-in for the SIP-4 precompile
/// @notice Install with {installMockZcash}: the runtime code is etched at
/// ZCASH_PRECOMPILE, so all state lives there and the scripting calls
/// below go to `MockZcash(ZCASH_PRECOMPILE)`.
///
/// Semantics mirror what the node must do (SIP-4 §2/§3):
/// - The segment is Z[base .. anchorHeight]. Blocks are implicit; hashes
///   are synthetic and change on every reorg.
/// - A tx added at a height above the anchor is "in flight": txInfo and
///   txOutput say NOT_FOUND until the anchor reaches it (never NOT_YET).
/// - confirmations = anchorHeight - height + 1.
/// - `reorg(h)` rewinds the anchor to h and drops every tx above h;
///   `removeTx` drops a single tx (it was reorged out / double-spent).
/// - `forceStatus` makes a txid return an arbitrary status, to exercise
///   library error paths (e.g. NOT_YET / OUT_OF_RANGE from a node that
///   chose to use them for txid lookups).
contract MockZcash is IZcash {
    struct Tx {
        bool exists;
        uint64 height;
        uint32 index;
        uint32 version;
    }

    struct Out {
        uint64 value;
        bytes script;
    }

    bool public initialized;
    uint64 public base;
    uint64 public anchorHeight;
    uint32 public baseTime;
    uint256 public reorgEpoch;

    mapping(bytes32 => Tx) internal txs;
    mapping(bytes32 => Out[]) internal outs;
    mapping(bytes32 => uint8) public forced; // 0 = not forced; else status + 1
    mapping(uint64 => bytes32[]) internal txsAt;
    mapping(bytes32 => address) internal burnCredited;
    mapping(bytes32 => uint32) internal burnSignal;
    mapping(bytes32 => uint64) internal burnWeight;

    // ------------------------------------------------------------------
    // Scripting
    // ------------------------------------------------------------------

    function init(uint64 base_, uint64 anchor_, uint32 baseTime_) external {
        require(!initialized, "MockZcash: init");
        require(anchor_ >= base_, "MockZcash: anchor < base");
        initialized = true;
        base = base_;
        anchorHeight = anchor_;
        baseTime = baseTime_;
    }

    /// @notice Advance the anchor by n Zcash blocks (one per Sova block).
    function mine(uint64 n) external {
        anchorHeight += n;
    }

    function setAnchor(uint64 h) external {
        require(h >= base, "MockZcash: below base");
        anchorHeight = h;
    }

    /// @notice Add a tx at `height` (may be above the anchor: in flight).
    function addTx(bytes32 txid, uint64 height, uint32 version) public {
        require(!txs[txid].exists, "MockZcash: dup txid");
        require(height >= base, "MockZcash: below base");
        uint32 idx = uint32(txsAt[height].length) + 1; // 0 is the coinbase
        txs[txid] = Tx({exists: true, height: height, index: idx, version: version});
        txsAt[height].push(txid);
    }

    function addOutput(bytes32 txid, uint64 valueZat, bytes memory script) public returns (uint32 vout) {
        require(txs[txid].exists, "MockZcash: no tx");
        vout = uint32(outs[txid].length);
        outs[txid].push(Out({value: valueZat, script: script}));
    }

    /// @notice One-output v5 payment mined in the NEXT block (anchor + 1).
    function pay(bytes32 txid, bytes memory script, uint64 valueZat) external returns (uint64 height) {
        height = anchorHeight + 1;
        addTx(txid, height, 5);
        addOutput(txid, valueZat, script);
    }

    function removeTx(bytes32 txid) public {
        delete txs[txid];
        delete outs[txid];
    }

    /// @notice Zcash reorg: rewind the anchor to h and drop txs above h.
    /// Heights above h are scanned up to `scanTo` (pass the old anchor).
    function reorg(uint64 h, uint64 scanTo) external {
        require(h >= base, "MockZcash: below base");
        for (uint64 k = h + 1; k <= scanTo; k++) {
            bytes32[] storage list = txsAt[k];
            for (uint256 i = 0; i < list.length; i++) {
                if (txs[list[i]].height == k) removeTx(list[i]);
            }
            delete txsAt[k];
        }
        anchorHeight = h;
        reorgEpoch++;
    }

    function forceStatus(bytes32 txid, uint8 status) external {
        forced[txid] = status + 1;
    }

    function setBurn(bytes32 txid, address credited, uint32 signal, uint64 weightZat) external {
        burnCredited[txid] = credited;
        burnSignal[txid] = signal;
        burnWeight[txid] = weightZat;
    }

    // ------------------------------------------------------------------
    // IZcash
    // ------------------------------------------------------------------

    function anchor() external view returns (uint64 height, bytes32 hash) {
        return (anchorHeight, _hashAt(anchorHeight));
    }

    function blockAt(uint64 h) external view returns (uint8 status, bytes32 hash, uint32 time) {
        if (h < base) return (ZcashStatus.OUT_OF_RANGE, bytes32(0), 0);
        if (h > anchorHeight) return (ZcashStatus.NOT_YET, bytes32(0), 0);
        return (ZcashStatus.OK, _hashAt(h), baseTime + uint32(h - base) * 75);
    }

    function txInfo(bytes32 txid)
        external
        view
        returns (uint8 status, uint64 height, uint32 index, uint64 confirmations, uint32 nOut, uint32 version)
    {
        uint8 f = forced[txid];
        if (f != 0) return (f - 1, 0, 0, 0, 0, 0);
        Tx storage t = txs[txid];
        if (!_visible(t)) return (ZcashStatus.NOT_FOUND, 0, 0, 0, 0, 0);
        return (ZcashStatus.OK, t.height, t.index, anchorHeight - t.height + 1, uint32(outs[txid].length), t.version);
    }

    function txOutput(bytes32 txid, uint32 vout)
        external
        view
        returns (uint8 status, uint64 valueZat, bytes memory script)
    {
        uint8 f = forced[txid];
        if (f != 0) return (f - 1, 0, "");
        Tx storage t = txs[txid];
        if (!_visible(t)) return (ZcashStatus.NOT_FOUND, 0, "");
        if (vout >= outs[txid].length) return (ZcashStatus.NO_SUCH_OUTPUT, 0, "");
        Out storage o = outs[txid][vout];
        return (ZcashStatus.OK, o.value, o.script);
    }

    function burnInfo(bytes32 txid)
        external
        view
        returns (uint8 status, address credited, uint32 signal, uint64 weightZat)
    {
        Tx storage t = txs[txid];
        if (!_visible(t)) return (ZcashStatus.NOT_FOUND, address(0), 0, 0);
        if (burnCredited[txid] == address(0)) return (ZcashStatus.NOT_A_BURN, address(0), 0, 0);
        return (ZcashStatus.OK, burnCredited[txid], burnSignal[txid], burnWeight[txid]);
    }

    function _visible(Tx storage t) internal view returns (bool) {
        return t.exists && t.height >= base && t.height <= anchorHeight;
    }

    function _hashAt(uint64 h) internal view returns (bytes32) {
        return keccak256(abi.encode("zcash-mock-block", h, reorgEpoch));
    }
}

/// @notice Etch MockZcash at the SIP-4 address and initialize it.
function installMockZcash(Vm vm, uint64 base, uint64 anchor_) returns (MockZcash z) {
    MockZcash impl = new MockZcash();
    vm.etch(ZCASH_PRECOMPILE, address(impl).code);
    z = MockZcash(ZCASH_PRECOMPILE);
    z.init(base, anchor_, 1_700_000_000);
}
