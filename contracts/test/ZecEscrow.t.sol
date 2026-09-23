// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {ZcashLib} from "../src/zcash/ZcashLib.sol";
import {ZecEscrow} from "../src/zcash/ZecEscrow.sol";
import {MockZcash, installMockZcash} from "./mocks/MockZcash.sol";

contract ZecEscrowTest is Test {
    uint64 constant BASE = 4_140_000;
    uint64 constant START = BASE + 1_000;

    uint64 constant PRICE = 250_000_000; // 2.5 ZEC
    uint128 constant BOND = 0.1 ether;
    uint64 constant WINDOW = 40; // ~50 min of Zcash blocks
    uint32 constant MINCONF = 3;
    uint256 constant AMOUNT = 10 ether;

    bytes20 constant PKH = bytes20(hex"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1");
    bytes20 constant PKH2 = bytes20(hex"b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2");
    bytes32 constant TX = keccak256("payment-1");
    bytes32 constant TX2 = keccak256("payment-2");

    MockZcash z;
    ZecEscrow esc;
    address maker = makeAddr("maker");
    address taker = makeAddr("taker");
    address taker2 = makeAddr("taker2");
    address rando = makeAddr("rando");

    function setUp() public {
        z = installMockZcash(vm, BASE, START);
        esc = new ZecEscrow();
        vm.deal(maker, 100 ether);
        vm.deal(taker, 10 ether);
        vm.deal(taker2, 10 ether);
    }

    // ---- helpers ----------------------------------------------------------

    function _create(bytes20 pkh) internal returns (uint256 id) {
        vm.prank(maker);
        id = esc.createOrder{value: AMOUNT}(PRICE, pkh, BOND, WINDOW, MINCONF);
    }

    function _reserve(uint256 id, address who) internal {
        vm.prank(who);
        esc.reserve{value: BOND}(id);
    }

    function _script(bytes20 pkh) internal pure returns (bytes memory) {
        return ZcashLib.p2pkh(pkh);
    }

    /// Pay in the next Zcash block, then let it reach `conf` confirmations.
    function _payAndConfirm(bytes32 txid, bytes20 pkh, uint64 value, uint64 conf) internal {
        z.pay(txid, _script(pkh), value);
        z.mine(conf);
    }

    function _claim(uint256 id, bytes32 txid) internal {
        vm.prank(taker);
        esc.claim(id, txid, 0);
    }

    // ---- happy path -------------------------------------------------------

    function testHappyPath() public {
        uint256 id = _create(PKH);
        assertEq(address(esc).balance, AMOUNT);
        _reserve(id, taker);
        assertTrue(esc.reservationLive(id));
        assertEq(esc.deadline(id), START + WINDOW);
        assertEq(esc.makerScript(id), _script(PKH));

        _payAndConfirm(TX, PKH, PRICE, MINCONF);
        uint256 before = taker.balance;
        _claim(id, TX);

        assertEq(taker.balance, before + AMOUNT + BOND);
        assertEq(address(esc).balance, 0);
        (,,,,, ZecEscrow.Status st,,,,) = esc.orders(id);
        assertEq(uint8(st), uint8(ZecEscrow.Status.Filled));
        assertEq(esc.paymentFilled(keccak256(abi.encode(TX, uint32(0)))), id);
        assertFalse(esc.pkhLive(PKH));
    }

    function testOverpaymentAccepted() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        _payAndConfirm(TX, PKH, PRICE * 2, MINCONF);
        _claim(id, TX);
    }

    function testThirdPartyClaimPaysTaker() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        _payAndConfirm(TX, PKH, PRICE, MINCONF);
        uint256 before = taker.balance;
        vm.prank(rando);
        esc.claim(id, TX, 0);
        assertEq(taker.balance, before + AMOUNT + BOND);
        assertEq(rando.balance, 0);
    }

    function testClaimAtLastWindowBlock() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        z.pay(TX, _script(PKH), PRICE);
        z.mine(WINDOW); // anchor == reservedAt + WINDOW: still live
        _claim(id, TX);
    }

    // ---- failures ---------------------------------------------------------

    function testUnderpayment() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        _payAndConfirm(TX, PKH, PRICE - 1, MINCONF);
        vm.expectRevert(
            abi.encodeWithSelector(ZcashLib.ZcashUnderpaid.selector, TX, uint32(0), uint64(PRICE - 1), PRICE)
        );
        _claim(id, TX);
    }

    function testWrongScript() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        _payAndConfirm(TX, PKH2, PRICE, MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashWrongScript.selector, TX, uint32(0)));
        _claim(id, TX);
    }

    function testInsufficientConfirmations() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        _payAndConfirm(TX, PKH, PRICE, MINCONF - 1);
        vm.expectRevert(
            abi.encodeWithSelector(
                ZcashLib.ZcashInsufficientConfirmations.selector, TX, uint64(MINCONF - 1), uint64(MINCONF)
            )
        );
        _claim(id, TX);
        z.mine(1);
        _claim(id, TX); // now deep enough
    }

    function testPaymentNotMinedYet() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        z.pay(TX, _script(PKH), PRICE); // in flight: above the anchor
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashTxNotFound.selector, TX));
        _claim(id, TX);
    }

    function testPaymentBeforeReservation() public {
        uint256 id = _create(PKH);
        _payAndConfirm(TX, PKH, PRICE, 1); // mined at START + 1
        _reserve(id, taker); // reservedAt = START + 1 (same height: not after)
        z.mine(MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZecEscrow.PaidBeforeReservation.selector, START + 1, START + 1));
        _claim(id, TX);
    }

    function testPaymentLongBeforeReservation() public {
        uint256 id = _create(PKH);
        z.addTx(TX, START - 10, 5);
        z.addOutput(TX, PRICE, _script(PKH));
        _reserve(id, taker);
        z.mine(MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZecEscrow.PaidBeforeReservation.selector, START - 10, START));
        _claim(id, TX);
    }

    function testClaimRequiresReservation() public {
        uint256 id = _create(PKH);
        _payAndConfirm(TX, PKH, PRICE, MINCONF);
        vm.expectRevert(ZecEscrow.NotReserved.selector);
        _claim(id, TX);
    }

    // ---- window expiry ----------------------------------------------------

    function testWindowExpiry() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        z.pay(TX, _script(PKH), PRICE);
        z.mine(WINDOW + 1); // one block too late to claim
        assertFalse(esc.reservationLive(id));
        vm.expectRevert(ZecEscrow.ReservationOver.selector);
        _claim(id, TX);

        uint256 makerBefore = maker.balance;
        vm.prank(rando);
        esc.expire(id);
        assertEq(maker.balance, makerBefore + BOND);
        assertEq(esc.deadline(id), 0);

        // Reopened: a new taker reserves and fills with a fresh payment.
        _reserve(id, taker2);
        _payAndConfirm(TX2, PKH, PRICE, MINCONF);
        uint256 t2 = taker2.balance;
        vm.prank(taker2);
        esc.claim(id, TX2, 0);
        assertEq(taker2.balance, t2 + AMOUNT + BOND);
    }

    function testReserveSettlesExpiredReservation() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        vm.prank(taker2);
        vm.expectRevert(ZecEscrow.AlreadyReserved.selector);
        esc.reserve{value: BOND}(id);

        z.mine(WINDOW + 1);
        uint256 makerBefore = maker.balance;
        _reserve(id, taker2);
        assertEq(maker.balance, makerBefore + BOND);
        assertEq(address(esc).balance, AMOUNT + BOND);
        (,,,,,,,, address t,) = esc.orders(id);
        assertEq(t, taker2);
    }

    function testExpireWhileLiveReverts() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        vm.expectRevert(ZecEscrow.ReservationLive.selector);
        esc.expire(id);
    }

    function testWrongBond() public {
        uint256 id = _create(PKH);
        vm.prank(taker);
        vm.expectRevert(ZecEscrow.WrongBond.selector);
        esc.reserve{value: BOND - 1}(id);
    }

    // ---- one payment, one fill -------------------------------------------

    function testSameAddressCannotBackTwoOpenOrders() public {
        _create(PKH);
        vm.prank(maker);
        vm.expectRevert(ZecEscrow.PkhInUse.selector);
        esc.createOrder{value: AMOUNT}(PRICE, PKH, BOND, WINDOW, MINCONF);
    }

    function testDoubleClaimAcrossTwoOrders() public {
        // Order 1 fills with TX.
        uint256 id1 = _create(PKH);
        _reserve(id1, taker);
        _payAndConfirm(TX, PKH, PRICE, MINCONF);
        _claim(id1, TX);

        // Maker reuses the (now free) address for order 2; the same payment
        // must not fill it, even for a reservation that precedes nothing.
        uint256 id2 = _create(PKH);
        _reserve(id2, taker);
        z.mine(MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZecEscrow.PaymentAlreadyUsed.selector, id1));
        _claim(id2, TX);
    }

    function testDoubleClaimSameOrder() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        _payAndConfirm(TX, PKH, PRICE, MINCONF);
        _claim(id, TX);
        vm.expectRevert(ZecEscrow.NotOpen.selector);
        _claim(id, TX);
    }

    function testOneTxTwoOutputsFillsTwoOrders() public {
        // Distinct (txid, vout) pairs are distinct payments.
        uint256 id1 = _create(PKH);
        uint256 id2 = _create(PKH2);
        _reserve(id1, taker);
        _reserve(id2, taker);
        z.addTx(TX, START + 1, 5);
        z.addOutput(TX, PRICE, _script(PKH));
        z.addOutput(TX, PRICE, _script(PKH2));
        z.mine(MINCONF);
        vm.startPrank(taker);
        esc.claim(id1, TX, 0);
        // vout 0 already filled order 1 (and pays the wrong script for order 2).
        vm.expectRevert(abi.encodeWithSelector(ZecEscrow.PaymentAlreadyUsed.selector, id1));
        esc.claim(id2, TX, 0);
        esc.claim(id2, TX, 1);
        vm.stopPrank();
    }

    // ---- reorg ------------------------------------------------------------

    function testReorgRemovesPayment() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        _payAndConfirm(TX, PKH, PRICE, MINCONF); // anchor = START + 3
        uint64 top = START + MINCONF;
        // Zcash reorgs back to the reservation height; the payment is gone
        // and the anchor moves on along the new branch without it.
        z.reorg(START, top);
        z.mine(MINCONF + 2);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashTxNotFound.selector, TX));
        _claim(id, TX);
    }

    function testReorgRemineShallower() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        _payAndConfirm(TX, PKH, PRICE, MINCONF);
        z.removeTx(TX); // reorged out ...
        z.addTx(TX, START + MINCONF, 5); // ... and re-mined higher
        z.addOutput(TX, PRICE, _script(PKH));
        vm.expectRevert(
            abi.encodeWithSelector(ZcashLib.ZcashInsufficientConfirmations.selector, TX, uint64(1), uint64(MINCONF))
        );
        _claim(id, TX);
    }

    // ---- cancel -----------------------------------------------------------

    function testCancelOpen() public {
        uint256 id = _create(PKH);
        uint256 before = maker.balance;
        vm.prank(maker);
        esc.cancel(id);
        assertEq(maker.balance, before + AMOUNT);
        assertFalse(esc.pkhLive(PKH));
        vm.expectRevert(ZecEscrow.NotOpen.selector);
        _reserve(id, taker);
    }

    function testCancelWhileReservedReverts() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        vm.prank(maker);
        vm.expectRevert(ZecEscrow.ReservationLive.selector);
        esc.cancel(id);
    }

    function testCancelAfterExpiryTakesBond() public {
        uint256 id = _create(PKH);
        _reserve(id, taker);
        z.mine(WINDOW + 1);
        uint256 before = maker.balance;
        vm.prank(maker);
        esc.cancel(id);
        assertEq(maker.balance, before + AMOUNT + BOND);
        assertEq(address(esc).balance, 0);
    }

    function testOnlyMakerCancels() public {
        uint256 id = _create(PKH);
        vm.prank(rando);
        vm.expectRevert(ZecEscrow.NotMaker.selector);
        esc.cancel(id);
    }

    // ---- create validation -------------------------------------------------

    function testCreateValidation() public {
        vm.startPrank(maker);
        vm.expectRevert(ZecEscrow.BadParams.selector);
        esc.createOrder(PRICE, PKH, BOND, WINDOW, MINCONF); // no value
        vm.expectRevert(ZecEscrow.BadParams.selector);
        esc.createOrder{value: 1}(0, PKH, BOND, WINDOW, MINCONF);
        vm.expectRevert(ZecEscrow.BadParams.selector);
        esc.createOrder{value: 1}(PRICE, bytes20(0), BOND, WINDOW, MINCONF);
        vm.expectRevert(ZecEscrow.BadParams.selector);
        esc.createOrder{value: 1}(PRICE, PKH, BOND, WINDOW, 0);
        vm.expectRevert(ZecEscrow.BadParams.selector);
        esc.createOrder{value: 1}(PRICE, PKH, BOND, MINCONF - 1, MINCONF);
        vm.expectRevert(ZecEscrow.BadParams.selector);
        esc.createOrder{value: 1}(PRICE, PKH, BOND, uint64(type(uint32).max) + 1, MINCONF);
        vm.stopPrank();
    }

    function testZeroBondOrder() public {
        vm.prank(maker);
        uint256 id = esc.createOrder{value: AMOUNT}(PRICE, PKH, 0, WINDOW, MINCONF);
        vm.prank(taker);
        esc.reserve(id);
        _payAndConfirm(TX, PKH, PRICE, MINCONF);
        uint256 before = taker.balance;
        _claim(id, TX);
        assertEq(taker.balance, before + AMOUNT);
    }

    // ---- fuzz -------------------------------------------------------------

    function testFuzzClaimOutcome(uint64 paid, uint64 payDelay, uint64 depth) public {
        paid = uint64(bound(paid, 1, 21_000_000e8));
        payDelay = uint64(bound(payDelay, 0, WINDOW)); // blocks after reservation before payment is mined
        depth = uint64(bound(depth, 1, WINDOW + 5));

        uint256 id = _create(PKH);
        _reserve(id, taker);
        z.mine(payDelay);
        z.pay(TX, _script(PKH), paid); // mined at START + payDelay + 1
        z.mine(depth); // confirmations == depth

        bool live = payDelay + depth <= WINDOW;
        bool ok = live && depth >= MINCONF && paid >= PRICE;

        uint256 before = taker.balance;
        vm.prank(taker);
        (bool success,) = address(esc).call(abi.encodeCall(ZecEscrow.claim, (id, TX, 0)));
        assertEq(success, ok);
        assertEq(taker.balance, ok ? before + AMOUNT + BOND : before);
        assertEq(address(esc).balance, ok ? 0 : AMOUNT + BOND);
    }

    function testFuzzPaymentHeightVsReservation(uint64 payHeightOffset) public {
        // Payment mined anywhere from 50 blocks before to 10 after the reservation.
        payHeightOffset = uint64(bound(payHeightOffset, 0, 60));
        uint64 payHeight = START - 50 + payHeightOffset;
        uint256 id = _create(PKH);
        z.addTx(TX, payHeight, 5);
        z.addOutput(TX, PRICE, _script(PKH));
        _reserve(id, taker); // reservedAt = START
        z.mine(WINDOW); // anchor = START + WINDOW, still live
        vm.prank(taker);
        (bool success,) = address(esc).call(abi.encodeCall(ZecEscrow.claim, (id, TX, 0)));
        assertEq(success, payHeight > START);
    }

    function testFuzzBalanceConservation(uint8 nOrders, uint8 fills) public {
        nOrders = uint8(bound(nOrders, 1, 12));
        fills = uint8(bound(fills, 0, nOrders));
        vm.deal(maker, uint256(nOrders) * AMOUNT);
        vm.deal(taker, uint256(nOrders) * BOND);
        for (uint256 i = 0; i < nOrders; i++) {
            _create(bytes20(uint160(i + 1)));
            _reserve(i + 1, taker);
        }
        for (uint256 i = 0; i < fills; i++) {
            z.pay(bytes32(i + 1), _script(bytes20(uint160(i + 1))), PRICE);
        }
        z.mine(MINCONF);
        for (uint256 i = 0; i < fills; i++) {
            _claim(i + 1, bytes32(i + 1));
        }
        uint256 open = uint256(nOrders) - fills;
        assertEq(address(esc).balance, open * (AMOUNT + BOND));
        assertEq(taker.balance, uint256(fills) * (AMOUNT + BOND)); // bonds all posted, filled ones returned
    }
}
