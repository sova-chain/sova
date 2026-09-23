// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {IZcash, ZcashStatus, ZCASH_PRECOMPILE} from "../src/zcash/IZcash.sol";
import {ZcashLib} from "../src/zcash/ZcashLib.sol";
import {MockZcash, installMockZcash} from "./mocks/MockZcash.sol";
import {TAddr} from "../script/ZecEscrowDemo.s.sol";

/// @dev External wrapper so library reverts can be caught with expectRevert.
contract LibHarness {
    function outputPays(bytes32 txid, uint32 vout, bytes memory script, uint64 minZat, uint64 minConf)
        external
        view
        returns (ZcashLib.Result r, ZcashLib.Payment memory p)
    {
        return ZcashLib.outputPays(txid, vout, script, minZat, minConf);
    }

    function requireOutputPays(bytes32 txid, uint32 vout, bytes memory script, uint64 minZat, uint64 minConf)
        external
        view
        returns (ZcashLib.Payment memory)
    {
        return ZcashLib.requireOutputPays(txid, vout, script, minZat, minConf);
    }

    function available() external view returns (bool) {
        return ZcashLib.available();
    }

    function p2pkh(bytes20 h) external pure returns (bytes memory) {
        return ZcashLib.p2pkh(h);
    }

    function p2sh(bytes20 h) external pure returns (bytes memory) {
        return ZcashLib.p2sh(h);
    }
}

contract ZcashLibTest is Test {
    uint64 constant BASE = 4_140_000;
    MockZcash z;
    LibHarness lib;

    bytes20 constant PKH = bytes20(hex"1111111111111111111111111111111111111111");
    bytes20 constant OTHER = bytes20(hex"2222222222222222222222222222222222222222");
    bytes32 constant TX = bytes32(uint256(0xabcd));

    function setUp() public {
        lib = new LibHarness();
        z = installMockZcash(vm, BASE, BASE + 100);
    }

    // ---- script builders -------------------------------------------------

    function testP2pkhLayout() public pure {
        bytes memory s = ZcashLib.p2pkh(PKH);
        assertEq(s, hex"76a914111111111111111111111111111111111111111188ac");
        assertEq(s.length, 25);
    }

    function testP2shLayout() public pure {
        bytes memory s = ZcashLib.p2sh(OTHER);
        assertEq(s, hex"a914222222222222222222222222222222222222222287");
        assertEq(s.length, 23);
    }

    function testFuzzScriptBuilders(bytes20 h) public pure {
        bytes memory a = ZcashLib.p2pkh(h);
        bytes memory b = ZcashLib.p2sh(h);
        assertEq(a.length, 25);
        assertEq(b.length, 23);
        bytes20 fromA;
        bytes20 fromB;
        assembly {
            fromA := mload(add(a, 35)) // 32 (len) + 3 (prefix)
            fromB := mload(add(b, 34)) // 32 (len) + 2 (prefix)
        }
        assertEq(fromA, h);
        assertEq(fromB, h);
        assertTrue(keccak256(a) != keccak256(b));
    }

    // ---- availability ----------------------------------------------------

    function testAvailable() public {
        assertTrue(lib.available());
        vm.etch(ZCASH_PRECOMPILE, "");
        assertFalse(lib.available());
    }

    function testCallsRevertWithoutPrecompile() public {
        vm.etch(ZCASH_PRECOMPILE, "");
        vm.expectRevert();
        lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
    }

    // ---- mock golden semantics (node side must match) ---------------------

    function testBlockAtHorizon() public view {
        (uint8 s,,) = IZcash(ZCASH_PRECOMPILE).blockAt(BASE - 1);
        assertEq(s, ZcashStatus.OUT_OF_RANGE);
        (s,,) = IZcash(ZCASH_PRECOMPILE).blockAt(BASE);
        assertEq(s, ZcashStatus.OK);
        (s,,) = IZcash(ZCASH_PRECOMPILE).blockAt(BASE + 100);
        assertEq(s, ZcashStatus.OK);
        (s,,) = IZcash(ZCASH_PRECOMPILE).blockAt(BASE + 101);
        assertEq(s, ZcashStatus.NOT_YET);
        (uint64 h, bytes32 ah) = IZcash(ZCASH_PRECOMPILE).anchor();
        (, bytes32 bh,) = IZcash(ZCASH_PRECOMPILE).blockAt(h);
        assertEq(h, BASE + 100);
        assertEq(ah, bh);
    }

    function testTxAboveAnchorIsNotFoundThenVisible() public {
        z.pay(TX, ZcashLib.p2pkh(PKH), 1e8); // mined at anchor + 1
        (uint8 s,,,,,) = IZcash(ZCASH_PRECOMPILE).txInfo(TX);
        assertEq(s, ZcashStatus.NOT_FOUND);
        z.mine(1);
        uint64 conf;
        uint32 nOut;
        (s,,, conf, nOut,) = IZcash(ZCASH_PRECOMPILE).txInfo(TX);
        assertEq(s, ZcashStatus.OK);
        assertEq(conf, 1);
        assertEq(nOut, 1);
        z.mine(9);
        (,,, conf,,) = IZcash(ZCASH_PRECOMPILE).txInfo(TX);
        assertEq(conf, 10);
    }

    function testCannotSendValue() public {
        (bool ok,) = ZCASH_PRECOMPILE.call{value: 1}(abi.encodeCall(IZcash.anchor, ()));
        assertFalse(ok);
    }

    // ---- outputPays / requireOutputPays -----------------------------------

    function _paid(uint64 value, uint64 extraBlocks) internal {
        z.pay(TX, ZcashLib.p2pkh(PKH), value);
        z.mine(1 + extraBlocks);
    }

    function testOk() public {
        _paid(5e8, 2); // 3 confirmations
        (ZcashLib.Result r, ZcashLib.Payment memory p) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 5e8, 3);
        assertEq(uint8(r), uint8(ZcashLib.Result.OK));
        assertEq(p.height, BASE + 101);
        assertEq(p.confirmations, 3);
        assertEq(p.valueZat, 5e8);
        ZcashLib.Payment memory q = lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 4e8, 1);
        assertEq(q.valueZat, 5e8);
    }

    function testMinConfZeroRejected() public {
        _paid(5e8, 5);
        vm.expectRevert(ZcashLib.ZcashMinConfZero.selector);
        lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 0);
        vm.expectRevert(ZcashLib.ZcashMinConfZero.selector);
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 0);
    }

    function testNotFound() public {
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
        assertEq(uint8(r), uint8(ZcashLib.Result.NOT_FOUND));
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashTxNotFound.selector, TX));
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
    }

    function testNotYetDistinct() public {
        z.forceStatus(TX, ZcashStatus.NOT_YET);
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
        assertEq(uint8(r), uint8(ZcashLib.Result.NOT_YET));
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashNotYet.selector, TX));
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
    }

    function testOutOfRangeDistinct() public {
        z.forceStatus(TX, ZcashStatus.OUT_OF_RANGE);
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
        assertEq(uint8(r), uint8(ZcashLib.Result.OUT_OF_RANGE));
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashOutOfRange.selector, TX));
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
    }

    function testBadStatus() public {
        z.forceStatus(TX, 9);
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
        assertEq(uint8(r), uint8(ZcashLib.Result.BAD_STATUS));
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashBadStatus.selector, uint8(9)));
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
    }

    function testNoSuchOutput() public {
        _paid(5e8, 5);
        (ZcashLib.Result r,) = lib.outputPays(TX, 1, ZcashLib.p2pkh(PKH), 1, 1);
        assertEq(uint8(r), uint8(ZcashLib.Result.NO_SUCH_OUTPUT));
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashNoSuchOutput.selector, TX, uint32(1)));
        lib.requireOutputPays(TX, 1, ZcashLib.p2pkh(PKH), 1, 1);
    }

    function testShieldedOnlyTxHasNoOutputs() public {
        z.addTx(TX, BASE + 50, 5); // no transparent outputs
        (uint8 s,,,, uint32 nOut,) = IZcash(ZCASH_PRECOMPILE).txInfo(TX);
        assertEq(s, ZcashStatus.OK);
        assertEq(nOut, 0);
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
        assertEq(uint8(r), uint8(ZcashLib.Result.NO_SUCH_OUTPUT));
    }

    function testWrongScript() public {
        _paid(5e8, 5);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashWrongScript.selector, TX, uint32(0)));
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(OTHER), 1, 1);
        // Same hash, different script type: still wrong.
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashWrongScript.selector, TX, uint32(0)));
        lib.requireOutputPays(TX, 0, ZcashLib.p2sh(PKH), 1, 1);
    }

    function testUnderpaid() public {
        _paid(5e8 - 1, 5);
        vm.expectRevert(
            abi.encodeWithSelector(ZcashLib.ZcashUnderpaid.selector, TX, uint32(0), uint64(5e8 - 1), uint64(5e8))
        );
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 5e8, 1);
    }

    function testInsufficientConfirmations() public {
        _paid(5e8, 1); // 2 confirmations
        vm.expectRevert(
            abi.encodeWithSelector(ZcashLib.ZcashInsufficientConfirmations.selector, TX, uint64(2), uint64(3))
        );
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 5e8, 3);
    }

    function testMultiOutputPicksVout() public {
        z.addTx(TX, BASE + 90, 5);
        z.addOutput(TX, 7e8, ZcashLib.p2pkh(OTHER)); // change
        z.addOutput(TX, 3e8, ZcashLib.p2pkh(PKH));
        lib.requireOutputPays(TX, 1, ZcashLib.p2pkh(PKH), 3e8, 11);
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 3e8, 11);
        assertEq(uint8(r), uint8(ZcashLib.Result.WRONG_SCRIPT));
    }

    function testReorgRemovesPayment() public {
        _paid(5e8, 5);
        lib.requireOutputPays(TX, 0, ZcashLib.p2pkh(PKH), 5e8, 3);
        z.reorg(BASE + 100, BASE + 106);
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 5e8, 3);
        assertEq(uint8(r), uint8(ZcashLib.Result.NOT_FOUND));
    }

    // ---- fuzz --------------------------------------------------------------

    function testFuzzValueThreshold(uint64 value, uint64 minZat) public {
        _paid(value, 9);
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), minZat, 10);
        if (value >= minZat) assertEq(uint8(r), uint8(ZcashLib.Result.OK));
        else assertEq(uint8(r), uint8(ZcashLib.Result.UNDERPAID));
    }

    function testFuzzConfirmations(uint16 extra, uint64 minConf) public {
        minConf = uint64(bound(minConf, 1, 5_000));
        _paid(1e8, extra);
        uint64 conf = uint64(extra) + 1;
        (ZcashLib.Result r, ZcashLib.Payment memory p) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1e8, minConf);
        assertEq(p.confirmations, conf);
        if (conf >= minConf) assertEq(uint8(r), uint8(ZcashLib.Result.OK));
        else assertEq(uint8(r), uint8(ZcashLib.Result.INSUFFICIENT_CONFIRMATIONS));
    }

    function testFuzzScriptMustMatchExactly(bytes memory script) public {
        vm.assume(keccak256(script) != keccak256(ZcashLib.p2pkh(PKH)));
        z.pay(TX, script, 1e8);
        z.mine(5);
        (ZcashLib.Result r,) = lib.outputPays(TX, 0, ZcashLib.p2pkh(PKH), 1, 1);
        assertEq(uint8(r), uint8(ZcashLib.Result.WRONG_SCRIPT));
    }
}

contract TAddrHarness {
    function decode(string memory a) external pure returns (bytes20, bool) {
        return TAddr.decodeP2pkh(a);
    }
}

/// Demo-script address codec, checked against real testnet addresses.
contract TAddrTest is Test {
    TAddrHarness h = new TAddrHarness();

    function testTestnetVectors() public view {
        (bytes20 p, bool t) = h.decode("tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrU");
        assertEq(p, bytes20(hex"00953dbc11a4ef7adcf6ba651bbb3b57430f3523"));
        assertTrue(t);
        assertEq(TAddr.encodeP2pkh(p, true), "tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrU");
        (p,) = h.decode("tmBBAmqyxawbM6Sf7NNNpqvBtQmbZTn45cV");
        assertEq(p, bytes20(hex"100a8b3e5fd8013867e509466c67ec391f177395"));
    }

    function testBadChecksumRejected() public {
        vm.expectRevert(bytes("TAddr: checksum"));
        h.decode("tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrV");
    }

    function testFuzzRoundTrip(bytes20 pkh, bool testnet) public view {
        string memory a = TAddr.encodeP2pkh(pkh, testnet);
        assertEq(bytes(a).length, 35);
        (bytes20 back, bool t) = h.decode(a);
        assertEq(back, pkh);
        assertEq(t, testnet);
    }
}
