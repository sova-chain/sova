// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Vm} from "forge-std/Vm.sol";
import {ZcashBlocks, ZCASH_BLOCKS, SYSTEM_ADDRESS} from "../../src/zcash/ZcashBlocks.sol";
import {MockZcashPools} from "./MockZcashPools.sol";

/// @title Test harness: plays the node's pre-block system call into ZcashBlocks
/// @notice `installZcashBlocks` etches the runtime bytecode at 0x…5A01 with
/// empty storage, exactly what the genesis alloc will hold. `systemRecord`
/// pranks SYSTEM_ADDRESS and sends the exact `record(Block)` calldata the
/// executor will send. `blockFromMock` builds that record from the mock
/// precompile's answers for one height, the way the node builds it from
/// the one index record it also serves through 0x…5A00.
library ZcashBlocksHarness {
    function installZcashBlocks(Vm vm) internal returns (ZcashBlocks zb) {
        ZcashBlocks impl = new ZcashBlocks();
        vm.etch(ZCASH_BLOCKS, address(impl).code);
        zb = ZcashBlocks(ZCASH_BLOCKS);
    }

    function systemRecord(Vm vm, ZcashBlocks zb, ZcashBlocks.Block memory b) internal {
        vm.prank(SYSTEM_ADDRESS);
        zb.record(b);
    }

    /// @notice The exact calldata the executor sends for `b`.
    function recordCalldata(ZcashBlocks.Block memory b) internal pure returns (bytes memory) {
        return abi.encodeCall(ZcashBlocks.record, (b));
    }

    function blockFromMock(MockZcashPools z, uint64 h) internal view returns (ZcashBlocks.Block memory b) {
        (uint8 st, bytes32 hash, uint32 time) = z.blockAt(h);
        require(st == 0, "harness: blockAt");
        b.height = h;
        b.hash = hash;
        b.time = time;
        (, uint64[] memory v, int64[] memory d) = z.poolTotals(h);
        for (uint256 i = 0; i < 6; i++) {
            b.pools[i] = v[i];
            b.deltas[i] = d[i];
        }
        MockZcashPools.Stats memory s = z.blockStats(h);
        b.txCount = s.txCount;
        b.shieldedTxCount = s.shieldedTxCount;
        b.tIn = s.tIn;
        b.tOut = s.tOut;
        b.saplingSpends = s.saplingSpends;
        b.saplingOutputs = s.saplingOutputs;
        b.orchardActions = s.orchardActions;
        b.ironwoodActions = s.ironwoodActions;
        b.joinSplits = s.joinSplits;
        b.notes[0] = s.saplingNotes;
        b.notes[1] = s.orchardNotes;
        b.notes[2] = s.ironwoodNotes;
    }
}
