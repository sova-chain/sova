// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, Vm, console2} from "forge-std/Test.sol";
import {ZcashStatus, ZCASH_PRECOMPILE} from "../src/zcash/IZcash.sol";
import {IZcashPools} from "../src/zcash/IZcashPools.sol";
import {ZcashBlocks, ZCASH_BLOCKS, SYSTEM_ADDRESS} from "../src/zcash/ZcashBlocks.sol";
import {MockZcashPools, installMockZcashPools} from "./mocks/MockZcashPools.sol";
import {ZcashBlocksHarness as H} from "./mocks/ZcashBlocksHarness.sol";

/// SIP-7 §4.1 ZcashBlocks: system-caller-only writes, exact storage layout,
/// ring wraparound, reads outside the window, publish idempotence, and
/// byte-for-byte parity with the precompile's height-keyed methods.
contract ZcashBlocksTest is Test {
    ZcashBlocks zb;

    // Real testnet totals at Zcash height 4,384,200 (SIP-7 Appendix A).
    uint64 constant H0 = 4_384_200;
    uint64[6] P0 = [
        uint64(1_573_837_835_978_306), // transparent
        42_832_983_037_484, // sprout
        152_869_428_798_703, // sapling
        23_913_312_221_154, // orchard
        15_894_393_750_000, // lockbox
        13_752_684_049_396 // ironwood
    ];

    bytes32 constant TOPIC = keccak256(
        "ZcashBlock(uint64,(uint64,bytes32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint64[6],int64[6],uint64[3]))"
    );

    function setUp() public {
        zb = H.installZcashBlocks(vm);
    }

    // ------------------------------------------------------------------
    // Builders
    // ------------------------------------------------------------------

    /// @dev Block h of a synthetic but realistic chain starting at H0 with
    /// P0: coinbase to transparent/lockbox/ironwood every block, plus a
    /// deterministic Sapling/Orchard wobble, so deltas are mixed-sign.
    function _chain(uint64 h) internal view returns (ZcashBlocks.Block memory b) {
        b.height = h;
        b.hash = keccak256(abi.encode("zcash", h));
        b.time = uint32(1_790_184_215 + (h - H0) * 75);
        uint64 k = h - H0;
        b.pools = _poolsAt(k);
        if (k > 0) {
            uint64[6] memory p = _poolsAt(k - 1);
            for (uint256 i = 0; i < 6; i++) {
                b.deltas[i] = int64(int256(uint256(b.pools[i])) - int256(uint256(p[i])));
            }
        } else {
            b.deltas = [int64(12_500_000), 0, 0, 0, 18_750_000, 125_000_000];
        }
        b.txCount = uint32(1 + (k % 5));
        b.shieldedTxCount = uint32(1 + (k % 3));
        b.tIn = uint32(k % 4);
        b.tOut = uint32(1 + (k % 6));
        b.saplingSpends = uint32(k % 2);
        b.saplingOutputs = uint32(1 + (k % 2));
        b.orchardActions = uint32((k % 4) * 2);
        b.ironwoodActions = uint32(2 + (k % 3) * 2);
        b.joinSplits = 0;
        b.notes = [uint64(404_304 + 2 * k), 248_902 + 3 * k, 354_039 + 4 * k];
    }

    function _poolsAt(uint64 k) internal view returns (uint64[6] memory p) {
        p = P0;
        p[0] += 12_500_000 * k;
        p[4] += 18_750_000 * k;
        p[5] += 125_000_000 * k;
        // wobble: sapling +/-, orchard only shrinks (NU6.3 rule)
        p[2] = uint64(int64(p[2]) + int64(int256(uint256(k % 7))) * 1_000_000 - int64(int256(uint256(k % 5))) * 900_000);
        p[3] -= 10_000 * k;
    }

    function _feed(uint64 from, uint64 to) internal {
        for (uint64 h = from; h <= to; h++) {
            H.systemRecord(vm, zb, _chain(h));
        }
    }

    function _eq(ZcashBlocks.Block memory a, ZcashBlocks.Block memory b) internal pure {
        assertEq(keccak256(abi.encode(a)), keccak256(abi.encode(b)), "block mismatch");
    }

    function _word(bytes memory ret, uint256 i) internal pure returns (uint256 w) {
        assembly {
            w := mload(add(ret, add(32, mul(i, 32))))
        }
    }

    // ------------------------------------------------------------------
    // Genesis / access
    // ------------------------------------------------------------------

    function test_genesisState() public {
        assertEq(vm.load(ZCASH_BLOCKS, 0), bytes32(0));
        (uint64 h, bytes32 hash) = zb.latest();
        assertEq(h, 0);
        assertEq(hash, bytes32(0));
        (uint64 lo, uint64 hi) = zb.window();
        assertEq(lo, 0);
        assertEq(hi, 0);
        (bool ok,) = zb.summary(H0);
        assertFalse(ok);
        (uint8 st,,) = zb.poolValue(H0, 2);
        assertEq(st, ZcashStatus.NOT_YET);
        (st,,) = zb.poolValue(0, 2);
        assertEq(st, ZcashStatus.NOT_YET);
        assertEq(zb.summaries(0, type(uint64).max).length, 0);
        assertEq(zb.publish(0, type(uint64).max), 0);
    }

    /// Genesis-deployable: the runtime code is self-contained (no
    /// constructor, no immutables), so two deployments have identical code
    /// and the etched copy works with zero storage.
    function test_runtimeCodeIsDeploymentIndependent() public {
        bytes memory a = address(new ZcashBlocks()).code;
        bytes memory b = address(new ZcashBlocks()).code;
        assertEq(keccak256(a), keccak256(b));
        assertEq(keccak256(ZCASH_BLOCKS.code), keccak256(a));
        console2.log("ZcashBlocks runtime bytes", a.length);
    }

    function testFuzz_onlySystemCanRecord(address caller) public {
        vm.assume(caller != SYSTEM_ADDRESS);
        ZcashBlocks.Block memory b = _chain(H0);
        vm.prank(caller);
        vm.expectRevert(ZcashBlocks.NotSystem.selector);
        zb.record(b);
    }

    function test_selectorIsDocumented() public pure {
        assertEq(ZcashBlocks.record.selector, bytes4(0x454a0745));
        assertEq(H.recordCalldata(_blank()).length, 4 + 27 * 32);
    }

    function _blank() internal pure returns (ZcashBlocks.Block memory b) {}

    // ------------------------------------------------------------------
    // Record / layout
    // ------------------------------------------------------------------

    function testFuzz_roundTrip(ZcashBlocks.Block memory b) public {
        for (uint256 i = 0; i < 3; i++) {
            b.notes[i] = uint64(bound(b.notes[i], 0, (1 << 40) - 1));
        }
        H.systemRecord(vm, zb, b);
        (bool ok, ZcashBlocks.Block memory got) = zb.summary(b.height);
        assertTrue(ok);
        _eq(got, b);
        for (uint8 p = 0; p < 6; p++) {
            (uint8 st, uint64 v, int64 d) = zb.poolValue(b.height, p);
            assertEq(st, ZcashStatus.OK);
            assertEq(v, b.pools[p]);
            assertEq(d, b.deltas[p]);
        }
    }

    function test_extremeValuesRoundTrip() public {
        ZcashBlocks.Block memory b;
        b.height = type(uint64).max;
        b.hash = bytes32(type(uint256).max);
        b.time = type(uint32).max;
        b.txCount = type(uint32).max;
        b.joinSplits = type(uint32).max;
        b.ironwoodActions = type(uint32).max;
        b.pools = [type(uint64).max, 0, 1, type(uint64).max, 0, type(uint64).max];
        b.deltas = [type(int64).min, type(int64).max, -1, 0, type(int64).min, -1];
        b.notes = [uint64((1 << 40) - 1), 0, (1 << 40) - 1];
        H.systemRecord(vm, zb, b);
        (bool ok, ZcashBlocks.Block memory got) = zb.summary(b.height);
        assertTrue(ok);
        _eq(got, b);
    }

    /// The documented slot formula and bit positions, read with
    /// eth_getStorageAt-equivalent loads.
    function test_storageLayout() public {
        ZcashBlocks.Block memory b = _chain(H0);
        b.deltas[3] = -7; // orchard negative, to check two's complement
        b.pools[3] = P0[3];
        H.systemRecord(vm, zb, b);
        uint256 head = uint256(vm.load(ZCASH_BLOCKS, 0));
        assertEq(uint64(head), H0, "newest");
        assertEq(uint64(head >> 64), H0, "first");
        assertEq((head >> 128) & 1, 1, "recorded flag");
        assertEq(head >> 129, 0);

        uint256 base = 1 + 6 * (uint256(H0) % 8191);
        uint256[6] memory s;
        for (uint256 j = 0; j < 6; j++) {
            s[j] = uint256(vm.load(ZCASH_BLOCKS, bytes32(base + j)));
        }
        assertEq(bytes32(s[0]), b.hash);
        assertEq(uint64(s[1]), H0);
        assertEq(uint32(s[1] >> 64), b.time);
        assertEq(uint32(s[1] >> 96), b.txCount);
        assertEq(uint32(s[1] >> 128), b.shieldedTxCount);
        assertEq(uint32(s[1] >> 160), b.tIn);
        assertEq(uint32(s[1] >> 192), b.tOut);
        assertEq(uint32(s[1] >> 224), b.joinSplits);
        for (uint256 i = 0; i < 4; i++) {
            assertEq(uint64(s[2] >> (64 * i)), b.pools[i]);
            assertEq(int64(uint64(s[4] >> (64 * i))), b.deltas[i]);
        }
        assertEq(uint64(s[4] >> 192), uint64(type(uint64).max) - 6, "-7 as u64");
        assertEq(uint64(s[3]), b.pools[4]);
        assertEq(uint64(s[3] >> 64), b.pools[5]);
        assertEq(uint32(s[3] >> 128), b.saplingSpends);
        assertEq(uint32(s[3] >> 160), b.saplingOutputs);
        assertEq(uint32(s[3] >> 192), b.orchardActions);
        assertEq(uint32(s[3] >> 224), b.ironwoodActions);
        assertEq(int64(uint64(s[5])), b.deltas[4]);
        assertEq(int64(uint64(s[5] >> 64)), b.deltas[5]);
        assertEq((s[5] >> 128) & ((1 << 40) - 1), b.notes[0]);
        assertEq((s[5] >> 168) & ((1 << 40) - 1), b.notes[1]);
        assertEq((s[5] >> 208) & ((1 << 40) - 1), b.notes[2]);
        assertEq(s[5] >> 248, 0, "unpublished");

        zb.publish(H0, H0);
        assertEq(uint256(vm.load(ZCASH_BLOCKS, bytes32(base + 5))) >> 248, 1, "published flag bit 248");
    }

    function test_continuity() public {
        _feed(H0, H0 + 2);
        ZcashBlocks.Block memory gap = _chain(H0 + 4);
        vm.prank(SYSTEM_ADDRESS);
        vm.expectRevert(abi.encodeWithSelector(ZcashBlocks.NotNext.selector, H0 + 3, H0 + 4));
        zb.record(gap);

        ZcashBlocks.Block memory again = _chain(H0 + 2);
        vm.prank(SYSTEM_ADDRESS);
        vm.expectRevert(abi.encodeWithSelector(ZcashBlocks.NotNext.selector, H0 + 3, H0 + 2));
        zb.record(again);
    }

    function test_deltaCrossCheck() public {
        _feed(H0, H0);
        ZcashBlocks.Block memory b = _chain(H0 + 1);
        b.deltas[5] += 1; // ironwood off by one zat
        vm.prank(SYSTEM_ADDRESS);
        vm.expectRevert(abi.encodeWithSelector(ZcashBlocks.DeltaMismatch.selector, uint8(5)));
        zb.record(b);

        // A wrong pool order (sapling/orchard swapped) is caught too.
        b = _chain(H0 + 1);
        (b.pools[2], b.pools[3]) = (b.pools[3], b.pools[2]);
        (b.deltas[2], b.deltas[3]) = (b.deltas[3], b.deltas[2]);
        vm.prank(SYSTEM_ADDRESS);
        vm.expectRevert(abi.encodeWithSelector(ZcashBlocks.DeltaMismatch.selector, uint8(2)));
        zb.record(b);
    }

    function test_notesBound() public {
        ZcashBlocks.Block memory b = _chain(H0);
        b.notes[1] = uint64(1 << 40);
        vm.prank(SYSTEM_ADDRESS);
        vm.expectRevert(ZcashBlocks.NotesTooLarge.selector);
        zb.record(b);
    }

    /// Strict decoding: a dirty uint32 word or a non sign-extended int64
    /// word reverts (the node must encode exactly).
    function test_dirtyCalldataReverts() public {
        bytes memory cd = H.recordCalldata(_chain(H0));
        bool ok;

        bytes memory bad = bytes.concat(cd);
        _setWord(bad, 3, uint256(1) << 32 | 1); // txCount with bit 32 set
        vm.prank(SYSTEM_ADDRESS);
        (ok,) = ZCASH_BLOCKS.call(bad);
        assertFalse(ok, "dirty uint32");

        bad = bytes.concat(cd);
        _setWord(bad, 18 + 3, uint256(uint64(type(uint64).max))); // -1 as u64, not sign-extended
        vm.prank(SYSTEM_ADDRESS);
        (ok,) = ZCASH_BLOCKS.call(bad);
        assertFalse(ok, "int64 not sign-extended");

        bad = bytes.concat(cd);
        _setWord(bad, 12, uint256(1) << 64); // pools[0] above uint64
        vm.prank(SYSTEM_ADDRESS);
        (ok,) = ZCASH_BLOCKS.call(bad);
        assertFalse(ok, "dirty uint64");

        // Short calldata reverts.
        vm.prank(SYSTEM_ADDRESS);
        (ok,) = ZCASH_BLOCKS.call(_slice(cd, cd.length - 32));
        assertFalse(ok, "short");

        vm.prank(SYSTEM_ADDRESS);
        (ok,) = ZCASH_BLOCKS.call(cd);
        assertTrue(ok, "clean");
    }

    function _setWord(bytes memory cd, uint256 w, uint256 v) internal pure {
        assembly {
            mstore(add(cd, add(36, mul(w, 32))), v)
        }
    }

    function _slice(bytes memory b, uint256 n) internal pure returns (bytes memory out) {
        out = new bytes(n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = b[i];
        }
    }

    // ------------------------------------------------------------------
    // Window / statuses
    // ------------------------------------------------------------------

    function test_statuses() public {
        _feed(H0, H0 + 9);
        (uint8 st,,) = zb.poolValue(H0 + 10, 2);
        assertEq(st, ZcashStatus.NOT_YET);
        (st,,) = zb.poolValue(H0 - 1, 2);
        assertEq(st, ZcashStatus.OUT_OF_RANGE);
        (st,,) = zb.poolValue(H0 + 5, 6);
        assertEq(st, ZcashStatus.NO_SUCH_POOL);
        (st,,) = zb.poolValue(H0 + 5, 255);
        assertEq(st, ZcashStatus.NO_SUCH_POOL);
        // Height status wins over pool status.
        (st,,) = zb.poolValue(H0 + 99, 6);
        assertEq(st, ZcashStatus.NOT_YET);
        (st,,) = zb.poolValue(H0 - 1, 6);
        assertEq(st, ZcashStatus.OUT_OF_RANGE);

        (uint8 s2, uint64[] memory v, int64[] memory d) = zb.poolTotals(H0 - 1);
        assertEq(s2, ZcashStatus.OUT_OF_RANGE);
        assertEq(v.length, 0);
        assertEq(d.length, 0);
        ZcashBlocks.Stats memory s = zb.blockStats(H0 + 10);
        assertEq(s.status, ZcashStatus.NOT_YET);
        assertEq(s.txCount, 0);

        (uint64 h, bytes32 hash) = zb.latest();
        assertEq(h, H0 + 9);
        assertEq(hash, _chain(H0 + 9).hash);
        (uint64 lo, uint64 hi) = zb.window();
        assertEq(lo, H0);
        assertEq(hi, H0 + 9);
    }

    /// Fill the ring past one full turn. Also reports the system call's
    /// cost while filling (zero -> nonzero) and once wrapped. Blocks are
    /// built incrementally (every field nonzero, every pool moving).
    function test_ringWrapAndRecordGas() public {
        uint64 first = H0;
        uint64 n = 8191 + 5;
        ZcashBlocks.Block memory b = _chain(first);
        H.systemRecord(vm, zb, b);
        b.deltas = [int64(1), 1, 1, -1, 1, 1];
        uint256 gFill;
        uint256 gWarm;
        vm.pauseGasMetering(); // a full ring turn is ~1.1B gas of SSTOREs
        for (uint64 k = 1; k < n; k++) {
            _step(b, first + k);
            bool measure = k == 100 || k == 8191 + 2;
            if (measure) vm.resumeGasMetering();
            vm.prank(SYSTEM_ADDRESS);
            uint256 g = gasleft();
            zb.record(b);
            g -= gasleft();
            if (measure) vm.pauseGasMetering();
            if (k == 100) gFill = g;
            if (k == 8191 + 2) gWarm = g;
        }
        vm.resumeGasMetering();
        uint64 newest = first + n - 1;
        (uint64 lo, uint64 hi) = zb.window();
        assertEq(hi, newest);
        assertEq(lo, newest - 8190);
        assertEq(hi - lo + 1, 8191);

        (bool ok,) = zb.summary(lo - 1);
        assertFalse(ok, "evicted");
        (uint8 st,,) = zb.poolValue(lo - 1, 0);
        assertEq(st, ZcashStatus.OUT_OF_RANGE);
        (ok,) = zb.summary(first);
        assertFalse(ok, "first evicted");

        ZcashBlocks.Block memory got;
        (ok, got) = zb.summary(lo);
        assertTrue(ok);
        assertEq(got.height, lo);
        assertEq(got.hash, bytes32(uint256(lo)));
        assertEq(got.pools[5], P0[5] + (lo - first));
        assertEq(got.pools[3], P0[3] - (lo - first));
        assertEq(got.deltas[3], -1);
        (ok, got) = zb.summary(newest);
        assertTrue(ok);
        _eq(got, b);
        // newest overwrote the slot of newest - 8191 (= lo - 1).
        assertEq(uint256(newest) % 8191, uint256(lo - 1) % 8191);

        console2.log("gas record (ring filling, cold zero->nonzero)", gFill);
        console2.log("gas record (ring wrapped, nonzero->nonzero)", gWarm);
    }

    function _step(ZcashBlocks.Block memory b, uint64 h) internal pure {
        b.height = h;
        b.hash = bytes32(uint256(h));
        b.time += 75;
        for (uint256 i = 0; i < 6; i++) {
            b.pools[i] = i == 3 ? b.pools[i] - 1 : b.pools[i] + 1;
        }
        b.notes[0] += 1;
        b.notes[1] += 2;
        b.notes[2] += 3;
        b.txCount = uint32(1 + h % 7);
    }

    // ------------------------------------------------------------------
    // Parity with the precompile
    // ------------------------------------------------------------------

    /// Feed ZcashBlocks from the mock precompile, as the node feeds it from
    /// the same index record; then every height-keyed answer must be
    /// byte-identical between 0x…5A00 and 0x…5A01.
    function test_parityWithPrecompile() public {
        uint64 base = H0;
        MockZcashPools z = installMockZcashPools(vm, base, base);
        z.setPools(base, P0, [int64(12_500_000), 0, 0, 0, 18_750_000, 125_000_000]);
        _stats(z, base);
        for (uint64 k = 1; k <= 30; k++) {
            z.pushPools(_poolsAt(k));
            _stats(z, base + k);
        }
        for (uint64 h = base; h <= base + 30; h++) {
            H.systemRecord(vm, zb, H.blockFromMock(z, h));
        }
        for (uint64 h = base - 1; h <= base + 31; h++) {
            _same(abi.encodeWithSelector(IZcashPools.poolTotals.selector, h));
            _same(abi.encodeWithSelector(IZcashPools.blockStats.selector, h));
            for (uint8 p = 0; p < 8; p++) {
                _same(abi.encodeWithSelector(IZcashPools.poolValue.selector, h, p));
            }
        }
    }

    function _stats(MockZcashPools z, uint64 h) internal {
        uint64 k = h - H0;
        z.setStats(
            h,
            MockZcashPools.Stats({
                status: 0,
                txCount: uint32(1 + k % 4),
                shieldedTxCount: uint32(1 + k % 2),
                tIn: uint32(k % 3),
                tOut: uint32(1 + k % 5),
                saplingSpends: uint32(k % 2),
                saplingOutputs: 1,
                orchardActions: uint32((k % 3) * 2),
                ironwoodActions: 2,
                joinSplits: uint32(k % 2),
                saplingNotes: 404_304 + k,
                orchardNotes: 248_902 + 2 * k,
                ironwoodNotes: 354_039 + 3 * k
            })
        );
    }

    function _same(bytes memory cd) internal view {
        (bool ok1, bytes memory a) = ZCASH_PRECOMPILE.staticcall(cd);
        (bool ok2, bytes memory b) = ZCASH_BLOCKS.staticcall(cd);
        assertTrue(ok1 && ok2, "call failed");
        assertEq(a, b, "precompile vs ZcashBlocks");
    }

    function test_selectorsMatchSip7() public pure {
        assertEq(IZcashPools.poolValue.selector, bytes4(0x0cd0bdbc));
        assertEq(IZcashPools.poolTotals.selector, bytes4(0x1c476c7e));
        assertEq(IZcashPools.blockStats.selector, bytes4(0x84df4c97));
        assertEq(IZcashPools.txShielded.selector, bytes4(0xdaa39583));
        assertEq(ZcashBlocks.poolValue.selector, IZcashPools.poolValue.selector);
        assertEq(ZcashBlocks.poolTotals.selector, IZcashPools.poolTotals.selector);
        assertEq(ZcashBlocks.blockStats.selector, IZcashPools.blockStats.selector);
        assertEq(MockZcashPools.blockStats.selector, IZcashPools.blockStats.selector);
        assertEq(MockZcashPools.txShielded.selector, IZcashPools.txShielded.selector);
    }

    /// blockStats returns a static struct; through the IZcashPools
    /// interface it must decode as the flat 13-value tuple.
    function test_blockStatsFlatAbi() public {
        _feed(H0, H0 + 1);
        (bool ok, bytes memory ret) = ZCASH_BLOCKS.staticcall(abi.encodeWithSelector(0x84df4c97, H0 + 1));
        assertTrue(ok);
        assertEq(ret.length, 13 * 32);
        ZcashBlocks.Block memory b = _chain(H0 + 1);
        assertEq(_word(ret, 0), 0);
        assertEq(_word(ret, 1), b.txCount);
        assertEq(_word(ret, 2), b.shieldedTxCount);
        assertEq(_word(ret, 3), b.tIn);
        assertEq(_word(ret, 4), b.tOut);
        assertEq(_word(ret, 5), b.saplingSpends);
        assertEq(_word(ret, 6), b.saplingOutputs);
        assertEq(_word(ret, 7), b.orchardActions);
        assertEq(_word(ret, 8), b.ironwoodActions);
        assertEq(_word(ret, 9), b.joinSplits);
        assertEq(_word(ret, 10), b.notes[0]);
        assertEq(_word(ret, 11), b.notes[1]);
        assertEq(_word(ret, 12), b.notes[2]);
        // Decodes as the interface's flat tuple.
        (uint8 st, uint32 txCount) = abi.decode(ret, (uint8, uint32));
        assertEq(st, 0);
        assertEq(txCount, b.txCount);
    }

    // ------------------------------------------------------------------
    // summaries
    // ------------------------------------------------------------------

    function test_summariesClamp() public {
        _feed(H0, H0 + 19);
        ZcashBlocks.Block[] memory r = zb.summaries(0, type(uint64).max);
        assertEq(r.length, 20);
        _eq(r[0], _chain(H0));
        _eq(r[19], _chain(H0 + 19));
        r = zb.summaries(H0 + 5, H0 + 7);
        assertEq(r.length, 3);
        assertEq(r[0].height, H0 + 5);
        assertEq(zb.summaries(H0 + 20, H0 + 30).length, 0);
        assertEq(zb.summaries(0, H0 - 1).length, 0);
        assertEq(zb.summaries(H0 + 7, H0 + 5).length, 0);
    }

    function test_summariesCap() public {
        _feed(H0, H0 + 1100);
        ZcashBlocks.Block[] memory r = zb.summaries(0, type(uint64).max);
        assertEq(r.length, 1024);
        assertEq(r[1023].height, H0 + 1100, "newest kept");
        assertEq(r[0].height, H0 + 1100 - 1023);
    }

    // ------------------------------------------------------------------
    // publish
    // ------------------------------------------------------------------

    function test_publishEmitsOnceAndIsIdempotent() public {
        _feed(H0, H0 + 4);
        vm.expectEmit(true, false, false, true, ZCASH_BLOCKS);
        emit ZcashBlocks.ZcashBlock(H0, _chain(H0));
        vm.expectEmit(true, false, false, true, ZCASH_BLOCKS);
        emit ZcashBlocks.ZcashBlock(H0 + 1, _chain(H0 + 1));
        assertEq(zb.publish(H0, H0 + 1), 2);
        assertTrue(zb.isPublished(H0));
        assertTrue(zb.isPublished(H0 + 1));
        assertFalse(zb.isPublished(H0 + 2));

        // Overlapping range: only the new heights.
        vm.recordLogs();
        assertEq(zb.publish(0, type(uint64).max), 3);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 3);
        for (uint256 i = 0; i < 3; i++) {
            assertEq(logs[i].topics[0], TOPIC);
            assertEq(uint256(logs[i].topics[1]), H0 + 2 + i);
            assertEq(logs[i].data, abi.encode(_chain(uint64(H0 + 2 + i))));
            assertEq(logs[i].emitter, ZCASH_BLOCKS);
        }
        assertEq(zb.publish(0, type(uint64).max), 0, "idempotent");

        // New record, publish again.
        _feed(H0 + 5, H0 + 5);
        assertFalse(zb.isPublished(H0 + 5));
        assertEq(zb.publish(H0, H0 + 5), 1);
        // Outside window / inverted.
        assertEq(zb.publish(H0 + 6, H0 + 100), 0);
        assertEq(zb.publish(H0 + 3, H0 + 1), 0);
        assertFalse(zb.isPublished(H0 + 6));
        assertFalse(zb.isPublished(H0 - 1));
    }

    /// A ring slot re-recorded after wraparound starts unpublished; the
    /// summary it held is gone.
    function test_publishFlagResetsOnWrap() public {
        uint64 first = 5_000_000;
        ZcashBlocks.Block memory b = _chain(H0);
        b.height = first;
        H.systemRecord(vm, zb, b);
        assertEq(zb.publish(first, first), 1);
        assertTrue(zb.isPublished(first));
        // Next heights continue from `first` with consistent pools.
        for (uint64 h = first + 1; h <= first + 8191; h++) {
            ZcashBlocks.Block memory n;
            n.height = h;
            n.pools = b.pools;
            H.systemRecord(vm, zb, n);
        }
        assertFalse(zb.isPublished(first), "evicted");
        assertFalse(zb.isPublished(first + 8191), "reused slot starts clear");
        assertEq(zb.publish(first + 8191, first + 8191), 1);
    }

    // ------------------------------------------------------------------
    // Gas
    // ------------------------------------------------------------------

    function test_gasReadsAndPublish() public {
        _feed(H0, H0 + 200);
        uint64 h = H0 + 150;
        uint256 g;

        g = gasleft();
        zb.latest();
        console2.log("gas latest()", g - gasleft());
        g = gasleft();
        zb.summary(h);
        console2.log("gas summary(h)", g - gasleft());
        g = gasleft();
        zb.poolValue(h, 5);
        console2.log("gas poolValue(h,pool)", g - gasleft());
        g = gasleft();
        zb.poolTotals(h);
        console2.log("gas poolTotals(h)", g - gasleft());
        g = gasleft();
        zb.blockStats(h);
        console2.log("gas blockStats(h)", g - gasleft());
        g = gasleft();
        zb.summaries(H0 + 105, H0 + 200);
        console2.log("gas summaries(96 blocks)", g - gasleft());

        g = gasleft();
        zb.publish(H0 + 200, H0 + 200);
        console2.log("gas publish(1 block)", g - gasleft());
        g = gasleft();
        zb.publish(H0 + 190, H0 + 199);
        console2.log("gas publish(10 blocks)", g - gasleft());
        g = gasleft();
        zb.publish(H0 + 190, H0 + 200);
        console2.log("gas publish(11 already published)", g - gasleft());
    }
}
