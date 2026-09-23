// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, console2} from "forge-std/Test.sol";
import {ZcashStatus} from "../src/zcash/IZcash.sol";
import {IZcashPools, ZcashPool} from "../src/zcash/IZcashPools.sol";
import {ZcashLib} from "../src/zcash/ZcashLib.sol";
import {ZcashBlocks} from "../src/zcash/ZcashBlocks.sol";
import {ZcashTrigger, ShieldedGrowthPrize} from "../src/zcash/ZcashTrigger.sol";
import {MockZcashPools, installMockZcashPools} from "./mocks/MockZcashPools.sol";
import {ZcashBlocksHarness as H} from "./mocks/ZcashBlocksHarness.sol";

/// External wrapper so library reverts can be expected.
contract PoolsReader {
    function requirePoolValue(uint64 h, uint8 pool) external view returns (uint64) {
        return ZcashLib.requirePoolValue(h, pool);
    }

    function poolDelta(uint64 h, uint8 pool) external view returns (uint8, int64) {
        return ZcashLib.poolDelta(h, pool);
    }

    function shieldedTotal(IZcashPools src, uint64 h) external view returns (uint8, uint64) {
        return ZcashLib.shieldedTotal(src, h);
    }

    function shieldedTotal(uint64 h) external view returns (uint8, uint64) {
        return ZcashLib.shieldedTotal(h);
    }

    function shieldedDelta(uint64 h) external view returns (uint8, int64) {
        return ZcashLib.shieldedDelta(h);
    }

    function poolChange(uint64 a, uint64 b, uint8 pool) external view returns (uint8, int256) {
        return ZcashLib.poolChange(a, b, pool);
    }

    function shieldedChange(uint64 a, uint64 b) external view returns (uint8, int256) {
        return ZcashLib.shieldedChange(a, b);
    }

    function crossedAbove(IZcashPools src, uint64 h, uint8 pool, uint64 t) external view returns (bool) {
        return ZcashLib.crossedAbove(src, h, pool, t);
    }

    function crossedBelow(IZcashPools src, uint64 h, uint8 pool, uint64 t) external view returns (bool) {
        return ZcashLib.crossedBelow(src, h, pool, t);
    }

    function shieldedCrossedAbove(IZcashPools src, uint64 h, uint64 t) external view returns (bool) {
        return ZcashLib.shieldedCrossedAbove(src, h, t);
    }

    function shieldedOutflow(bytes32 txid) external view returns (uint8, uint64, int256) {
        return ZcashLib.shieldedOutflow(txid);
    }
}

/// Repeating trigger example (test-only): fires each time Ironwood falls
/// below `floor`, at most once per `cooldown` Zcash blocks.
contract IronwoodFloorAlarm is ZcashTrigger {
    uint64 public immutable floor;
    uint64 public immutable cooldown;

    constructor(uint64 floor_, uint64 cooldown_, uint256 bounty_, uint64 minConf_, uint64 start_)
        payable
        ZcashTrigger(bounty_, minConf_, start_)
    {
        floor = floor_;
        cooldown = cooldown_;
    }

    function _armed(uint64 h) internal view override returns (bool) {
        return fires == 0 || h >= lastFiredAt + cooldown;
    }

    function condition(uint64 h) public view override returns (bool) {
        return ZcashLib.crossedBelow(ZcashLib.POOLS, h, ZcashPool.IRONWOOD, floor);
    }

    function _act(uint64) internal override {}
}

/// Keeper that tries to re-enter poke from its bounty receipt.
contract GreedyKeeper {
    ZcashTrigger t;
    uint64 h;

    function go(ZcashTrigger t_, uint64 h_) external {
        (t, h) = (t_, h_);
        t_.poke(h_);
    }

    receive() external payable {
        t.poke(h);
    }
}

contract ZcashPoolsTest is Test {
    MockZcashPools z;
    PoolsReader r;

    uint64 constant BASE = 4_384_000;
    // Real testnet totals at 4,384,200 (SIP-7 Appendix A), used at BASE.
    uint64[6] P0 = [
        uint64(1_573_837_835_978_306),
        42_832_983_037_484,
        152_869_428_798_703,
        23_913_312_221_154,
        15_894_393_750_000,
        13_752_684_049_396
    ];
    uint64 shielded0;

    function setUp() public {
        z = installMockZcashPools(vm, BASE, BASE);
        z.setPools(BASE, P0, [int64(12_500_000), 0, 0, 0, 18_750_000, 125_000_000]);
        r = new PoolsReader();
        shielded0 = P0[1] + P0[2] + P0[3] + P0[5];
    }

    /// Mine one coinbase-only block (SIP-7 Appendix A deltas) with an
    /// extra `shield` zat moved from transparent into Sapling.
    function _mine(int64 shield) internal returns (uint64 h) {
        (, uint64[] memory v,) = z.poolTotals(z.anchorHeight());
        uint64[6] memory n;
        for (uint256 i = 0; i < 6; i++) {
            n[i] = v[i];
        }
        n[0] = uint64(int64(n[0]) + 12_500_000 - shield);
        n[2] = uint64(int64(n[2]) + shield);
        n[4] += 18_750_000;
        n[5] += 125_000_000;
        h = z.pushPools(n);
    }

    // ------------------------------------------------------------------
    // Library helpers
    // ------------------------------------------------------------------

    function test_statusesPassThrough() public {
        (uint8 st,) = r.poolDelta(BASE - 1, ZcashPool.SAPLING);
        assertEq(st, ZcashStatus.OUT_OF_RANGE);
        (st,) = r.poolDelta(BASE + 1, ZcashPool.SAPLING);
        assertEq(st, ZcashStatus.NOT_YET);
        (st,) = r.poolDelta(BASE, 6);
        assertEq(st, ZcashStatus.NO_SUCH_POOL);
        (st,) = r.shieldedTotal(BASE + 1);
        assertEq(st, ZcashStatus.NOT_YET);
        (st,) = r.shieldedDelta(BASE - 1);
        assertEq(st, ZcashStatus.OUT_OF_RANGE);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashPoolRead.selector, BASE + 1, ZcashStatus.NOT_YET));
        r.requirePoolValue(BASE + 1, ZcashPool.SAPLING);
        assertEq(r.requirePoolValue(BASE, ZcashPool.IRONWOOD), P0[5]);
    }

    /// A pool not yet defined answers NO_SUCH_POOL, not a revert.
    function test_noSuchPool() public {
        z.setPoolCount(5); // pretend Ironwood (id 5) is not defined yet
        (uint8 st, int64 d) = r.poolDelta(BASE, ZcashPool.IRONWOOD);
        assertEq(st, ZcashStatus.NO_SUCH_POOL);
        assertEq(d, 0);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashPoolRead.selector, BASE, ZcashStatus.NO_SUCH_POOL));
        r.requirePoolValue(BASE, ZcashPool.IRONWOOD);
    }

    function test_shieldedTotalAndDelta() public {
        (uint8 st, uint64 total) = r.shieldedTotal(BASE);
        assertEq(st, 0);
        assertEq(total, P0[1] + P0[2] + P0[3] + P0[5]);
        uint64 h = _mine(40_000_000);
        (, int64 d) = r.shieldedDelta(h);
        assertEq(d, 125_000_000 + 40_000_000, "ironwood coinbase + sapling shield");
        (, int64 dt) = r.poolDelta(h, ZcashPool.TRANSPARENT);
        assertEq(dt, 12_500_000 - 40_000_000);
        (, int64 dl) = r.poolDelta(h, ZcashPool.LOCKBOX);
        assertEq(dl, 18_750_000);
        assertTrue(ZcashPool.isShielded(ZcashPool.ORCHARD));
        assertFalse(ZcashPool.isShielded(ZcashPool.LOCKBOX));
        assertFalse(ZcashPool.isShielded(ZcashPool.TRANSPARENT));
    }

    function test_windowChange() public {
        for (uint256 i = 0; i < 10; i++) {
            _mine(1_000_000);
        }
        (uint8 st, int256 c) = r.poolChange(BASE, BASE + 10, ZcashPool.IRONWOOD);
        assertEq(st, 0);
        assertEq(c, 10 * 125_000_000);
        (st, c) = r.poolChange(BASE + 10, BASE, ZcashPool.IRONWOOD);
        assertEq(c, -10 * 125_000_000, "reversed window is negative");
        (st, c) = r.shieldedChange(BASE, BASE + 10);
        assertEq(c, 10 * (125_000_000 + 1_000_000));
        (st,) = r.poolChange(BASE - 1, BASE + 10, ZcashPool.IRONWOOD);
        assertEq(st, ZcashStatus.OUT_OF_RANGE);
        (st,) = r.shieldedChange(BASE, BASE + 11);
        assertEq(st, ZcashStatus.NOT_YET);
    }

    function test_crossings() public {
        IZcashPools src = IZcashPools(address(z));
        uint64 line = P0[5] + 3 * 125_000_000; // Ironwood reaches it at BASE+3
        for (uint256 i = 0; i < 5; i++) {
            _mine(0);
        }
        assertFalse(r.crossedAbove(src, BASE + 2, ZcashPool.IRONWOOD, line));
        assertTrue(r.crossedAbove(src, BASE + 3, ZcashPool.IRONWOOD, line));
        assertFalse(r.crossedAbove(src, BASE + 4, ZcashPool.IRONWOOD, line), "already above");
        assertFalse(r.crossedAbove(src, BASE, ZcashPool.IRONWOOD, 0), "h-1 out of range");
        assertFalse(r.crossedAbove(src, BASE + 9, ZcashPool.IRONWOOD, line), "not yet");
        assertFalse(r.crossedAbove(src, 0, ZcashPool.IRONWOOD, line));

        // Orchard shrinking below a line.
        uint64[6] memory v = P0;
        v[5] = P0[5] + 6 * 125_000_000;
        v[3] = P0[3] - 1_000;
        z.pushPools(v); // BASE+6
        assertTrue(r.crossedBelow(src, BASE + 6, ZcashPool.ORCHARD, P0[3]));
        assertFalse(r.crossedBelow(src, BASE + 5, ZcashPool.ORCHARD, P0[3]));

        uint64 sline = shielded0 + 2 * 125_000_000;
        assertTrue(r.shieldedCrossedAbove(src, BASE + 2, sline));
        assertFalse(r.shieldedCrossedAbove(src, BASE + 1, sline));
        assertFalse(r.shieldedCrossedAbove(src, BASE + 3, sline));
    }

    /// Same helpers over the ZcashBlocks ring give the same answers.
    function test_helpersOverZcashBlocks() public {
        ZcashBlocks zb = H.installZcashBlocks(vm);
        for (uint256 i = 0; i < 6; i++) {
            _mine(int64(int256(i)) * 3_000_000);
        }
        for (uint64 h = BASE; h <= BASE + 6; h++) {
            H.systemRecord(vm, zb, H.blockFromMock(z, h));
        }
        IZcashPools ring = IZcashPools(address(zb));
        IZcashPools pre = IZcashPools(address(z));
        for (uint64 h = BASE; h <= BASE + 6; h++) {
            (uint8 s1, uint64 a) = r.shieldedTotal(pre, h);
            (uint8 s2, uint64 b) = r.shieldedTotal(ring, h);
            assertEq(s1, s2);
            assertEq(a, b);
        }
        uint64 sline = shielded0 + 2 * 125_000_000;
        assertEq(r.shieldedCrossedAbove(ring, BASE + 2, sline), r.shieldedCrossedAbove(pre, BASE + 2, sline));
    }

    /// SIP-7 Appendix A, block 4,384,160: a deshield out of Ironwood and a
    /// coinbase paid to Sapling.
    function test_shieldedOutflow() public {
        uint64 h = _mine(0);
        bytes32 deshield = bytes32(uint256(0x47c0));
        bytes32 coinbase = bytes32(uint256(0x5057));
        bytes32 plain = bytes32(uint256(0xabcd));
        z.addTx(deshield, h, 6);
        z.addTx(coinbase, h, 6);
        z.addTx(plain, h, 5);
        z.setTxShielded(deshield, [int64(0), 0, 0, -1_025_000], 0, [uint32(0), 0, 0, 4, 0]);
        z.setTxShielded(coinbase, [int64(0), 125_035_000, 0, 0], 1, [uint32(0), 1, 0, 0, 0]);

        (uint8 st, uint64 at, int256 out) = r.shieldedOutflow(deshield);
        assertEq(st, 0);
        assertEq(at, h);
        assertEq(out, 1_025_000, "0.01 TAZ paid out + fee left Ironwood");
        (,, out) = r.shieldedOutflow(coinbase);
        assertEq(out, -125_035_000, "value entered Sapling");
        (st,, out) = r.shieldedOutflow(plain);
        assertEq(st, 0);
        assertEq(out, 0, "transparent-only tx");
        (st,,) = r.shieldedOutflow(bytes32(uint256(1)));
        assertEq(st, ZcashStatus.NOT_FOUND);

        // In flight (mined above the anchor): NOT_FOUND until anchored.
        bytes32 later = bytes32(uint256(0x1a7e));
        z.addTx(later, h + 1, 6);
        (st,,) = r.shieldedOutflow(later);
        assertEq(st, ZcashStatus.NOT_FOUND);
    }

    // ------------------------------------------------------------------
    // ZcashTrigger / ShieldedGrowthPrize
    // ------------------------------------------------------------------

    address constant BENEFICIARY = address(0xB0B);
    address constant KEEPER = address(0x4EE9);

    function _prize(uint256 fund, uint64 start) internal returns (ShieldedGrowthPrize p) {
        // Crosses at BASE+3: +125M Ironwood per block.
        uint64 line = shielded0 + 3 * 125_000_000;
        p = new ShieldedGrowthPrize{value: fund}(line, BENEFICIARY, 0.1 ether, 3, start);
    }

    function test_triggerFiresOnceAndPays() public {
        ShieldedGrowthPrize p = _prize(1 ether, BASE + 1);
        for (uint256 i = 0; i < 4; i++) {
            _mine(0);
        } // anchor BASE+4: BASE+3 has 2 confs
        assertFalse(p.ready(BASE + 3));
        vm.expectRevert(abi.encodeWithSelector(ZcashTrigger.TooShallow.selector, BASE + 3, BASE + 4, 3));
        vm.prank(KEEPER);
        p.poke(BASE + 3);

        _mine(0); // BASE+5: 3 confs
        vm.expectRevert(abi.encodeWithSelector(ZcashTrigger.ConditionNotMet.selector, BASE + 2));
        vm.prank(KEEPER);
        p.poke(BASE + 2);
        vm.expectRevert(abi.encodeWithSelector(ZcashTrigger.TooShallow.selector, BASE + 4, BASE + 5, 3));
        p.poke(BASE + 4); // depth is checked before the condition

        assertTrue(p.ready(BASE + 3));
        vm.expectEmit(true, true, false, true, address(p));
        emit ZcashTrigger.Fired(BASE + 3, KEEPER, 0.1 ether);
        vm.prank(KEEPER);
        uint256 g = gasleft();
        p.poke(BASE + 3);
        console2.log("gas poke (fires, 2 transfers; mock precompile)", g - gasleft());

        assertEq(KEEPER.balance, 0.1 ether, "bounty");
        assertEq(BENEFICIARY.balance, 0.9 ether, "prize");
        assertEq(address(p).balance, 0);
        assertEq(p.fires(), 1);
        assertEq(p.lastFiredAt(), BASE + 3);
        assertFalse(p.ready(BASE + 3));

        vm.expectRevert(abi.encodeWithSelector(ZcashTrigger.NotArmed.selector, BASE + 3));
        p.poke(BASE + 3);
    }

    function test_triggerRejectsOldWitness() public {
        ShieldedGrowthPrize p = _prize(1 ether, BASE + 4);
        for (uint256 i = 0; i < 8; i++) {
            _mine(0);
        }
        vm.expectRevert(abi.encodeWithSelector(ZcashTrigger.TooEarly.selector, BASE + 3, BASE + 4));
        p.poke(BASE + 3);
    }

    function test_triggerUnderfunded() public {
        ShieldedGrowthPrize p = _prize(0.05 ether, BASE);
        for (uint256 i = 0; i < 6; i++) {
            _mine(0);
        }
        vm.prank(KEEPER);
        p.poke(BASE + 3);
        assertEq(KEEPER.balance, 0.05 ether, "keeper gets what is left");
        assertEq(BENEFICIARY.balance, 0);
    }

    function test_triggerTopUpAndNoReentry() public {
        ShieldedGrowthPrize p = _prize(0, BASE);
        (bool ok,) = address(p).call{value: 1 ether}("");
        assertTrue(ok, "receive");
        for (uint256 i = 0; i < 6; i++) {
            _mine(0);
        }
        GreedyKeeper k = new GreedyKeeper();
        vm.expectRevert(ZcashTrigger.BountyFailed.selector);
        k.go(p, BASE + 3);
        assertEq(p.fires(), 0, "rolled back");
        assertEq(address(p).balance, 1 ether);
    }

    function test_triggerMinConfZero() public {
        vm.expectRevert(ZcashTrigger.MinConfZero.selector);
        new ShieldedGrowthPrize(1, BENEFICIARY, 0, 0, 0);
    }

    function test_repeatingTrigger() public {
        // Ironwood dips below P0 at BASE+2 and again at BASE+12.
        IronwoodFloorAlarm a = new IronwoodFloorAlarm{value: 1 ether}(P0[5], 5, 0.01 ether, 1, BASE);
        uint64[6] memory v = P0;
        for (uint64 k = 1; k <= 14; k++) {
            v[5] = (k == 2 || k == 12) ? P0[5] - 1 : P0[5];
            z.pushPools(v);
        }
        vm.prank(KEEPER);
        a.poke(BASE + 2);
        vm.expectRevert(abi.encodeWithSelector(ZcashTrigger.NotArmed.selector, BASE + 6));
        a.poke(BASE + 6);
        vm.prank(KEEPER);
        a.poke(BASE + 12);
        assertEq(a.fires(), 2);
        assertEq(KEEPER.balance, 0.02 ether);
    }
}
