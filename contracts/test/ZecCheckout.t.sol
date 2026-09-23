// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, console2} from "forge-std/Test.sol";
import {stdStorage, StdStorage} from "forge-std/StdStorage.sol";
import {Ashwings} from "../src/Ashwings.sol";
import {ZcashLib} from "../src/zcash/ZcashLib.sol";
import {ZecCheckout, AshwingsZecCheckout} from "../src/zcash/ZecCheckout.sol";
import {ZCASH_PRECOMPILE} from "../src/zcash/IZcash.sol";
import {MockZcash, installMockZcash} from "./mocks/MockZcash.sol";
import {ZecFmt} from "../script/AshwingZecCheckoutDemo.s.sol";

contract ZecCheckoutTest is Test {
    using stdStorage for StdStorage;

    uint64 constant BASE = 4_140_000;
    uint64 constant START = BASE + 1_000;

    uint64 constant PRICE = 25_000_000; // 0.25 ZEC
    uint32 constant WINDOW = 40; // ~50 min of Zcash blocks
    uint16 constant MINCONF = 3;
    uint64 constant TAGS = 100_000; // == ZecCheckout.TAG_SPACE (checked in setUp)

    bytes20 constant PKH = bytes20(hex"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1");
    bytes20 constant PKH2 = bytes20(hex"b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2");
    bytes32 constant TX = keccak256("payment-1");
    bytes32 constant TX2 = keccak256("payment-2");

    MockZcash z;
    Ashwings ash;
    AshwingsZecCheckout co;
    address seller = makeAddr("seller");
    address buyer = makeAddr("buyer");
    address buyer2 = makeAddr("buyer2");
    address relayer = makeAddr("relayer");

    function setUp() public {
        z = installMockZcash(vm, BASE, START);
        ash = new Ashwings();
        co = new AshwingsZecCheckout(address(ash));
        assertEq(co.TAG_SPACE(), TAGS);
    }

    // ---- helpers ----------------------------------------------------------

    function _list() internal returns (uint256) {
        vm.prank(seller);
        return co.list(PRICE, PKH, false, WINDOW, MINCONF);
    }

    function _reserve(uint256 listingId, address who) internal returns (uint256) {
        vm.prank(who);
        return co.reserve(listingId, who);
    }

    function _s(bytes20 pkh) internal pure returns (bytes memory) {
        return ZcashLib.p2pkh(pkh);
    }

    function _claim(uint256 rid, bytes32 txid, uint32 vout) internal returns (uint256) {
        vm.prank(relayer);
        return co.claim(rid, txid, vout);
    }

    // ---- happy path -------------------------------------------------------

    function testHappyPath() public {
        uint256 lid = _list();
        uint256 rid = _reserve(lid, buyer);
        assertEq(co.quote(rid), PRICE + 1);
        assertEq(co.payeeScript(rid), _s(PKH));
        assertEq(co.deadline(rid), START + WINDOW);

        z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(MINCONF);
        uint256 id = _claim(rid, TX, 0); // a relayer claims; the owl goes to the buyer

        assertEq(ash.ownerOf(id), buyer);
        assertEq(ash.balanceOf(buyer), 1);
        assertEq(ash.balanceOf(address(co)), 0);
        assertEq(ash.balanceOf(relayer), 0);
        assertEq(co.paymentUsed(keccak256(abi.encode(TX, uint32(0)))), rid);
        (,,,, bool filled,,,) = co.reservations(rid);
        assertTrue(filled);
    }

    function testP2shPayee() public {
        vm.prank(seller);
        uint256 lid = co.list(PRICE, PKH, true, WINDOW, MINCONF);
        uint256 rid = _reserve(lid, buyer);
        assertEq(co.payeeScript(rid), ZcashLib.p2sh(PKH));
        z.pay(TX, ZcashLib.p2sh(PKH), PRICE + 1);
        z.mine(MINCONF);
        _claim(rid, TX, 0);
    }

    /// A buyer with no SOVA: a relayer reserves for them and claims for them.
    function testGaslessRelay() public {
        uint256 lid = _list();
        vm.prank(relayer);
        uint256 rid = co.reserve(lid, buyer);
        z.pay(TX, _s(PKH), co.quote(rid));
        z.mine(MINCONF);
        uint256 id = _claim(rid, TX, 0);
        assertEq(ash.ownerOf(id), buyer);
    }

    /// Payment mined at the last window height, confirmed after the window: fine.
    function testMinedAtDeadlineConfirmedLater() public {
        uint256 lid = _list();
        uint256 rid = _reserve(lid, buyer);
        z.mine(WINDOW - 1);
        uint64 h = z.pay(TX, _s(PKH), PRICE + 1);
        assertEq(h, START + WINDOW);
        z.mine(MINCONF + 100);
        _claim(rid, TX, 0);
    }

    function testPaymentAmongOtherOutputs() public {
        uint256 lid = _list();
        uint256 rid = _reserve(lid, buyer);
        z.addTx(TX, START + 1, 5);
        z.addOutput(TX, 12_345, _s(PKH2)); // change
        uint32 vout = z.addOutput(TX, PRICE + 1, _s(PKH));
        z.mine(MINCONF);
        _claim(rid, TX, vout);
    }

    // ---- payment failures -------------------------------------------------

    function testUnderpay() public {
        uint256 rid = _reserve(_list(), buyer);
        z.pay(TX, _s(PKH), PRICE); // forgot the tag
        z.mine(MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashUnderpaid.selector, TX, uint32(0), PRICE, PRICE + 1));
        _claim(rid, TX, 0);
    }

    function testOverpay() public {
        uint256 rid = _reserve(_list(), buyer);
        z.pay(TX, _s(PKH), PRICE + 2);
        z.mine(MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZecCheckout.WrongAmount.selector, PRICE + 2, PRICE + 1));
        _claim(rid, TX, 0);
    }

    function testWrongScript() public {
        uint256 rid = _reserve(_list(), buyer);
        z.pay(TX, _s(PKH2), PRICE + 1);
        z.mine(MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashWrongScript.selector, TX, uint32(0)));
        _claim(rid, TX, 0);
    }

    function testWrongScriptTypeSameHash() public {
        uint256 rid = _reserve(_list(), buyer); // P2PKH listing
        z.pay(TX, ZcashLib.p2sh(PKH), PRICE + 1);
        z.mine(MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashWrongScript.selector, TX, uint32(0)));
        _claim(rid, TX, 0);
    }

    function testTooFewConfirmations() public {
        uint256 rid = _reserve(_list(), buyer);
        z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(MINCONF - 1);
        vm.expectRevert(
            abi.encodeWithSelector(
                ZcashLib.ZcashInsufficientConfirmations.selector, TX, uint64(MINCONF - 1), uint64(MINCONF)
            )
        );
        _claim(rid, TX, 0);
        z.mine(1);
        _claim(rid, TX, 0);
    }

    function testInFlightNotFound() public {
        uint256 rid = _reserve(_list(), buyer);
        z.pay(TX, _s(PKH), PRICE + 1); // above the anchor
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashTxNotFound.selector, TX));
        _claim(rid, TX, 0);
    }

    function testPaymentBeforeReservation() public {
        uint256 lid = _list();
        // Tags are predictable: a payment made before reserving is refused.
        z.pay(TX, _s(PKH), PRICE + 1); // mined at START + 1
        z.mine(1);
        uint256 rid = _reserve(lid, buyer); // reservedAt = START + 1
        z.mine(MINCONF);
        vm.expectRevert(abi.encodeWithSelector(ZecCheckout.PaidBeforeReservation.selector, START + 1, START + 1));
        _claim(rid, TX, 0);
    }

    function testWindowExpiry() public {
        uint256 rid = _reserve(_list(), buyer);
        z.mine(WINDOW); // anchor = START + WINDOW; next block is past the deadline
        z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(MINCONF);
        vm.expectRevert(
            abi.encodeWithSelector(ZecCheckout.PaidAfterDeadline.selector, START + WINDOW + 1, START + WINDOW)
        );
        _claim(rid, TX, 0);
    }

    /// The late payment is not claimable by the NEXT reserver either
    /// (the ZecEscrow reuse hazard): its amount matches only its own tag.
    function testLatePaymentNotStealableByNextReserver() public {
        uint256 lid = _list();
        uint256 r1 = _reserve(lid, buyer);
        z.mine(WINDOW);
        uint256 r2 = _reserve(lid, buyer2);
        z.pay(TX, _s(PKH), co.quote(r1)); // buyer pays late
        z.mine(MINCONF);
        vm.expectRevert(
            abi.encodeWithSelector(ZecCheckout.PaidAfterDeadline.selector, START + WINDOW + 1, START + WINDOW)
        );
        _claim(r1, TX, 0);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashUnderpaid.selector, TX, uint32(0), PRICE + 1, PRICE + 2));
        _claim(r2, TX, 0);
    }

    // ---- one payment, one fill --------------------------------------------

    function testDoubleClaim() public {
        uint256 lid = _list();
        uint256 r1 = _reserve(lid, buyer);
        uint256 r2 = _reserve(lid, buyer2);
        z.pay(TX, _s(PKH), co.quote(r1));
        z.mine(MINCONF);
        _claim(r1, TX, 0);

        vm.expectRevert(ZecCheckout.AlreadyFilled.selector);
        _claim(r1, TX, 0);
        vm.expectRevert(abi.encodeWithSelector(ZecCheckout.PaymentAlreadyUsed.selector, r1));
        _claim(r2, TX, 0);
        assertEq(ash.totalSupply(), 1);
    }

    function testSameReservationSecondPaymentRefused() public {
        uint256 rid = _reserve(_list(), buyer);
        z.pay(TX, _s(PKH), PRICE + 1);
        z.pay(TX2, _s(PKH), PRICE + 1); // buyer paid twice by mistake
        z.mine(MINCONF);
        _claim(rid, TX, 0);
        vm.expectRevert(ZecCheckout.AlreadyFilled.selector);
        _claim(rid, TX2, 0);
    }

    function testUnknownReservation() public {
        vm.expectRevert(ZecCheckout.NoSuchReservation.selector);
        _claim(7, TX, 0);
    }

    // ---- shared payee address (amount tags) -------------------------------

    function testTwoBuyersShareOneAddress() public {
        uint256 lid = _list();
        uint256 r1 = _reserve(lid, buyer);
        uint256 r2 = _reserve(lid, buyer2);
        assertEq(co.quote(r1), PRICE + 1);
        assertEq(co.quote(r2), PRICE + 2);
        assertEq(co.payeeScript(r1), co.payeeScript(r2));

        z.pay(TX, _s(PKH), PRICE + 1);
        z.pay(TX2, _s(PKH), PRICE + 2);
        z.mine(MINCONF);

        // Neither can take the other's payment.
        vm.expectRevert(abi.encodeWithSelector(ZecCheckout.WrongAmount.selector, PRICE + 2, PRICE + 1));
        _claim(r1, TX2, 0);
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashUnderpaid.selector, TX, uint32(0), PRICE + 1, PRICE + 2));
        _claim(r2, TX, 0);

        // Out of order is fine.
        uint256 a2 = _claim(r2, TX2, 0);
        uint256 a1 = _claim(r1, TX, 0);
        assertEq(ash.ownerOf(a1), buyer);
        assertEq(ash.ownerOf(a2), buyer2);
    }

    /// Tags never repeat for a (payee, price), even across listings and
    /// price round-trips, so no two reservations ever share a quote.
    function testTagsUniqueAcrossListingsAndPriceChanges() public {
        uint256 l1 = _list();
        vm.prank(buyer2); // anyone may list, even naming someone else's address
        uint256 l2 = co.list(PRICE, PKH, false, WINDOW, MINCONF);

        assertEq(co.quote(_reserve(l1, buyer)), PRICE + 1);
        assertEq(co.quote(_reserve(l2, buyer)), PRICE + 2);

        vm.prank(seller);
        co.update(l1, PRICE + TAGS, PKH, false, WINDOW, MINCONF, true);
        assertEq(co.quote(_reserve(l1, buyer)), PRICE + TAGS + 1);
        vm.prank(seller);
        co.update(l1, PRICE, PKH, false, WINDOW, MINCONF, true);
        assertEq(co.quote(_reserve(l1, buyer)), PRICE + 3);

        vm.prank(seller);
        co.update(l1, PRICE, PKH2, false, WINDOW, MINCONF, true);
        assertEq(co.quote(_reserve(l1, buyer)), PRICE + 1); // new address, own counter
    }

    function testTagsExhausted() public {
        uint256 lid = _list();
        bytes32 key = keccak256(abi.encode(false, PKH, PRICE));
        stdstore.target(address(co)).sig(co.lastTag.selector).with_key(key).checked_write(uint256(TAGS - 2));
        assertEq(co.quote(_reserve(lid, buyer)), PRICE + TAGS - 1); // last tag
        vm.prank(buyer);
        vm.expectRevert(ZecCheckout.TagsExhausted.selector);
        co.reserve(lid, buyer);
    }

    // ---- reorg ------------------------------------------------------------

    function testReorgRemovesPayment() public {
        uint256 rid = _reserve(_list(), buyer);
        uint64 h = z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(MINCONF);
        uint64 top = START + MINCONF;

        z.reorg(h - 1, top); // Zcash drops the block with the payment
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashTxNotFound.selector, TX));
        _claim(rid, TX, 0);

        // Re-mined on the new branch, still inside the window: claimable.
        z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(MINCONF);
        _claim(rid, TX, 0);
    }

    function testReorgRemovesTxOnly() public {
        uint256 rid = _reserve(_list(), buyer);
        z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(MINCONF);
        z.removeTx(TX); // double-spent away on the winning branch
        vm.expectRevert(abi.encodeWithSelector(ZcashLib.ZcashTxNotFound.selector, TX));
        _claim(rid, TX, 0);
    }

    function testReorgRemintedPastDeadline() public {
        uint256 rid = _reserve(_list(), buyer);
        uint64 h = z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(WINDOW);
        z.reorg(h - 1, START + WINDOW + 1);
        z.setAnchor(START + WINDOW + 5);
        z.addTx(TX, START + WINDOW + 5, 5); // re-mined late
        z.addOutput(TX, PRICE + 1, _s(PKH));
        z.mine(MINCONF);
        vm.expectRevert(
            abi.encodeWithSelector(ZecCheckout.PaidAfterDeadline.selector, START + WINDOW + 5, START + WINDOW)
        );
        _claim(rid, TX, 0);
    }

    // ---- listings ---------------------------------------------------------

    function testOnlySellerUpdates() public {
        uint256 lid = _list();
        vm.prank(buyer);
        vm.expectRevert(ZecCheckout.NotSeller.selector);
        co.update(lid, PRICE, PKH2, false, WINDOW, MINCONF, true);
        vm.expectRevert(ZecCheckout.NotSeller.selector);
        co.update(99, PRICE, PKH2, false, WINDOW, MINCONF, true);
    }

    function testPausedListing() public {
        uint256 lid = _list();
        uint256 rid = _reserve(lid, buyer);
        vm.prank(seller);
        co.update(lid, PRICE, PKH, false, WINDOW, MINCONF, false);
        vm.prank(buyer2);
        vm.expectRevert(ZecCheckout.ListingInactive.selector);
        co.reserve(lid, buyer2);
        // An existing reservation still settles.
        z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(MINCONF);
        _claim(rid, TX, 0);
    }

    function testEditsDoNotTouchReservations() public {
        uint256 lid = _list();
        uint256 rid = _reserve(lid, buyer);
        vm.prank(seller);
        co.update(lid, PRICE * 4, PKH2, true, 1, 50, true);
        assertEq(co.quote(rid), PRICE + 1);
        assertEq(co.payeeScript(rid), _s(PKH));
        assertEq(co.deadline(rid), START + WINDOW);
        z.pay(TX, _s(PKH), PRICE + 1);
        z.mine(MINCONF);
        _claim(rid, TX, 0);
    }

    function testUnknownListing() public {
        vm.expectRevert(ZecCheckout.ListingInactive.selector);
        co.reserve(5, buyer);
    }

    function testZeroRecipient() public {
        uint256 lid = _list();
        vm.expectRevert(ZecCheckout.ZeroRecipient.selector);
        co.reserve(lid, address(0));
    }

    function testBadParams() public {
        vm.startPrank(seller);
        vm.expectRevert(ZecCheckout.BadParams.selector);
        co.list(0, PKH, false, WINDOW, MINCONF);
        vm.expectRevert(ZecCheckout.BadParams.selector);
        co.list(PRICE + 1, PKH, false, WINDOW, MINCONF); // not a multiple of TAG_SPACE
        vm.expectRevert(ZecCheckout.BadParams.selector);
        co.list(PRICE, bytes20(0), false, WINDOW, MINCONF);
        vm.expectRevert(ZecCheckout.BadParams.selector);
        co.list(PRICE, PKH, false, 0, MINCONF);
        vm.expectRevert(ZecCheckout.BadParams.selector);
        co.list(PRICE, PKH, false, WINDOW, 0);
        vm.stopPrank();
    }

    // ---- gas --------------------------------------------------------------

    /// Claim gas against the mock, and an estimate at SIP-4 prices: the mock
    /// is ordinary EVM code, so its own cost is measured and swapped for the
    /// SIP-4 §4 table (txInfo 4,000 + txOutput 4,000 + 8 x 25 bytes; a
    /// precompile is always warm). Excludes the 21,000 base and calldata.
    function testGasClaim() public {
        uint256 lid = _list();
        uint256 r0 = _reserve(lid, buyer); // warm the Ashwings supply slot like a live chain
        z.pay(TX2, _s(PKH), PRICE + 1);
        z.mine(MINCONF);
        _claim(r0, TX2, 0);

        vm.cool(ZCASH_PRECOMPILE);
        vm.cool(address(co));
        uint256 g = gasleft();
        vm.prank(buyer2);
        uint256 rid = co.reserve(lid, buyer2);
        uint256 reserveGas = g - gasleft();

        z.pay(TX, _s(PKH), co.quote(rid));
        z.mine(MINCONF);

        // Measure as a fresh transaction would see it: cold accounts and slots.
        vm.cool(ZCASH_PRECOMPILE);
        g = gasleft();
        z.txInfo(TX);
        z.txOutput(TX, 0);
        uint256 mockGas = g - gasleft();

        vm.cool(ZCASH_PRECOMPILE);
        vm.cool(address(co));
        vm.cool(address(ash));
        g = gasleft();
        co.claim(rid, TX, 0);
        uint256 claimGas = g - gasleft();

        uint256 sip4 = 4_000 + 4_000 + 8 * 25;
        console2.log("reserve gas (mock anchor)", reserveGas);
        console2.log("claim gas (mock precompile)", claimGas);
        console2.log("mock txInfo+txOutput gas", mockGas);
        console2.log("claim gas est. at SIP-4 prices", claimGas - mockGas + sip4);
        assertLt(claimGas, 250_000);
    }
}

contract ZecFmtTest is Test {
    function testZecFormatting() public pure {
        assertEq(ZecFmt.zec(25_000_001), "0.25000001");
        assertEq(ZecFmt.zec(1), "0.00000001");
        assertEq(ZecFmt.zec(1_234_500_042), "12.34500042");
        assertEq(
            ZecFmt.uri("tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrU", 25_000_001),
            "zcash:tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrU?amount=0.25000001"
        );
    }
}
