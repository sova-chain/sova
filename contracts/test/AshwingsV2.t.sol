// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, console2} from "forge-std/Test.sol";
import {stdStorage, StdStorage} from "forge-std/StdStorage.sol";
import {Ashwings} from "../src/Ashwings.sol";
import {AshwingsZecCheckout} from "../src/AshwingsZecCheckout.sol";
import {AshwingsMarket} from "../src/AshwingsMarket.sol";
import {ZecCheckout} from "../src/zcash/ZecCheckout.sol";
import {ZcashLib} from "../src/zcash/ZcashLib.sol";
import {ZCASH_PRECOMPILE} from "../src/zcash/IZcash.sol";
import {MockZcash, installMockZcash} from "./mocks/MockZcash.sol";
import {ZcashAddress} from "../src/zcash/ZcashAddress.sol";
import {TAddr} from "../script/ZecEscrowDemo.s.sol";

/// Shared fixture: Ashwings (10 SOVA / 0.25 ZEC), its ZEC checkout, the
/// market, and the SIP-4 mock precompile.
abstract contract AshwingsFixture is Test {
    uint64 constant BASE = 4_140_000;
    uint64 constant START = BASE + 1_000;
    uint256 constant PRICE_WEI = 10 ether;
    uint64 constant PRICE_ZAT = 25_000_000;
    uint32 constant WINDOW = 40;
    uint16 constant MINCONF = 3;
    bytes20 constant PAYEE = bytes20(hex"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1");
    address constant TREASURY = 0x8d0123637062f8c15FD6AFDe6F834C85f4AfeEDE;

    MockZcash z;
    Ashwings ash;
    AshwingsZecCheckout co;
    AshwingsMarket market;
    string payeeT; // PAYEE as a testnet t-address (tm...)

    address alice = makeAddr("alice");
    address bob = makeAddr("bob");
    address carol = makeAddr("carol");
    address relayer = makeAddr("relayer");

    uint256 txSeq;

    function setUp() public virtual {
        z = installMockZcash(vm, BASE, START);
        payeeT = TAddr.encodeP2pkh(PAYEE, true);
        ash = new Ashwings(TREASURY, payeeT, PRICE_WEI, PRICE_ZAT);
        co = ash.zecCheckout();
        market = new AshwingsMarket(address(ash), TREASURY, 100);
        vm.deal(alice, 1_000 ether);
        vm.deal(bob, 1_000 ether);
        vm.deal(carol, 1_000 ether);
    }

    function _mint(address who) internal returns (uint256) {
        vm.prank(who);
        return ash.mint{value: PRICE_WEI}();
    }

    function _reserve(address recipient) internal returns (uint256) {
        vm.prank(relayer);
        return co.reserve(1, recipient);
    }

    /// Pay reservation `rid`'s exact quote in the next Zcash block.
    function _pay(uint256 rid) internal returns (bytes32 txid) {
        txid = keccak256(abi.encode("zec-payment", ++txSeq));
        z.pay(txid, ZcashLib.p2pkh(PAYEE), co.quote(rid));
    }

    function _claim(uint256 rid, bytes32 txid) internal returns (uint256) {
        vm.prank(relayer);
        return co.claim(rid, txid, 0);
    }

    /// Jump the collection to `n` minted (totalSupply is slot 0).
    function _setSupply(uint256 n) internal {
        vm.store(address(ash), bytes32(uint256(0)), bytes32(n));
    }
}

contract AshwingsMintTest is AshwingsFixture {
    using stdStorage for StdStorage;

    // ---- terms --------------------------------------------------------------

    function testTermsAreConstructorArgs() public view {
        assertEq(ash.MAX_SUPPLY(), 10_000);
        assertEq(ash.priceWei(), PRICE_WEI);
        assertEq(ash.priceZat(), PRICE_ZAT);
        assertEq(ash.treasury(), TREASURY);
        assertEq(ash.zecPayee(), payeeT);
        assertEq(address(co.ashwings()), address(ash));
        (address seller, uint64 priceZat, uint16 minConf, bool p2sh, bool active, bytes20 payee, uint32 window) =
            co.listings(1);
        assertEq(seller, address(co));
        assertEq(priceZat, PRICE_ZAT);
        assertEq(minConf, MINCONF);
        assertFalse(p2sh);
        assertTrue(active);
        assertEq(payee, PAYEE);
        assertEq(window, WINDOW);
        assertEq(co.listingCount(), 1);
        assertEq(co.holdBlocks(), WINDOW + MINCONF + co.CLAIM_GRACE());
        // The wrap-around tag argument needs every live order to have its own tag.
        assertLt(ash.MAX_SUPPLY(), co.TAG_SPACE() - 1);
    }

    function testZecTermsCannotChange() public {
        vm.expectRevert(AshwingsZecCheckout.Immutable.selector);
        co.list(PRICE_ZAT, PAYEE, false, WINDOW, MINCONF);
        vm.expectRevert(AshwingsZecCheckout.Immutable.selector);
        co.update(1, 1e5, bytes20(uint160(7)), false, WINDOW, MINCONF, true);
        vm.prank(address(co)); // not even the listing's own "seller"
        vm.expectRevert(AshwingsZecCheckout.Immutable.selector);
        co.update(1, 1e5, bytes20(uint160(7)), false, WINDOW, MINCONF, false);
        vm.expectRevert(ZecCheckout.ListingInactive.selector);
        co.reserve(2, alice);
    }

    function testConstructorChecks() public {
        vm.expectRevert(bytes("ASHW: zero treasury"));
        new Ashwings(address(0), payeeT, PRICE_WEI, PRICE_ZAT);
        vm.expectRevert(ZecCheckout.BadParams.selector); // not a multiple of 0.001 ZEC
        new Ashwings(TREASURY, payeeT, PRICE_WEI, PRICE_ZAT + 1);
        vm.expectRevert(ZecCheckout.BadParams.selector);
        new Ashwings(TREASURY, payeeT, PRICE_WEI, 0);
        vm.expectRevert(bytes("ASHW: priceZat too large"));
        new Ashwings(TREASURY, payeeT, PRICE_WEI, uint256(type(uint64).max) + 1);
        vm.expectRevert(ZcashAddress.BadZcashAddress.selector); // checksum
        new Ashwings(TREASURY, "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNW", PRICE_WEI, PRICE_ZAT);
        vm.expectRevert(ZcashAddress.BadZcashAddress.selector); // not base58
        new Ashwings(TREASURY, "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxN0", PRICE_WEI, PRICE_ZAT);
        vm.expectRevert(ZcashAddress.BadZcashAddress.selector); // a Sova address is not a payee
        new Ashwings(TREASURY, "0x8d0123637062f8c15FD6AFDe6F834C85f4AfeEDE", PRICE_WEI, PRICE_ZAT);
    }

    /// The payee's network picks the payment depth; P2SH payees work too.
    function testPayeeNetworkAndType() public {
        bytes20 h = bytes20(uint160(0xBEEF));
        Ashwings main = new Ashwings(TREASURY, _b58(0x1cb8, h), PRICE_WEI, PRICE_ZAT); // t1
        (,, uint16 minConf, bool p2sh,, bytes20 payee,) = main.zecCheckout().listings(1);
        assertEq(minConf, 10);
        assertFalse(p2sh);
        assertEq(payee, h);
        Ashwings t3 = new Ashwings(TREASURY, _b58(0x1cbd, h), PRICE_WEI, PRICE_ZAT); // t3
        (,, minConf, p2sh,,,) = t3.zecCheckout().listings(1);
        assertEq(minConf, 10);
        assertTrue(p2sh);
        Ashwings t2 = new Ashwings(TREASURY, _b58(0x1cba, h), PRICE_WEI, PRICE_ZAT); // t2
        (,, minConf, p2sh,,,) = t2.zecCheckout().listings(1);
        assertEq(minConf, 3);
        assertTrue(p2sh);
        (,, minConf, p2sh,,,) = co.listings(1); // tm
        assertEq(minConf, 3);
        assertFalse(p2sh);
    }

    /// Base58Check with any 2-byte prefix (test helper).
    function _b58(uint16 prefix, bytes20 h) internal pure returns (string memory) {
        bytes memory alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        bytes memory payload = abi.encodePacked(prefix, h);
        uint256 n =
            uint256(uint208(bytes26(abi.encodePacked(payload, bytes4(sha256(abi.encodePacked(sha256(payload))))))));
        bytes memory out = new bytes(36);
        uint256 i = 36;
        while (n > 0) {
            out[--i] = alphabet[n % 58];
            n /= 58;
        }
        bytes memory r = new bytes(36 - i);
        for (uint256 k = 0; k < r.length; k++) {
            r[k] = out[i + k];
        }
        return string(r);
    }

    function testOnlyCheckoutMovesZecSupply() public {
        vm.startPrank(alice);
        vm.expectRevert(bytes("ASHW: not the checkout"));
        ash.holdForZec();
        vm.expectRevert(bytes("ASHW: not the checkout"));
        ash.releaseZecHolds(0);
        vm.expectRevert(bytes("ASHW: not the checkout"));
        ash.mintForZec(alice, false);
        vm.stopPrank();
    }

    // ---- SOVA path -----------------------------------------------------------

    function testSovaMint() public {
        uint256 id = _mint(alice);
        assertEq(id, 1);
        assertEq(ash.ownerOf(1), alice);
        assertEq(address(ash).balance, PRICE_WEI);
        assertTrue(ash.seedOf(1) != bytes32(0));
    }

    function testSovaMintExactPriceOnly() public {
        vm.startPrank(alice);
        vm.expectRevert(bytes("ASHW: wrong price"));
        ash.mint{value: PRICE_WEI - 1}();
        vm.expectRevert(bytes("ASHW: wrong price"));
        ash.mint{value: PRICE_WEI + 1}();
        vm.expectRevert(bytes("ASHW: wrong price"));
        ash.mint();
        vm.stopPrank();
    }

    function testWithdrawGoesOnlyToTreasury() public {
        _mint(alice);
        _mint(bob);
        uint256 before = TREASURY.balance;
        vm.prank(carol); // anyone may trigger it
        ash.withdraw();
        assertEq(TREASURY.balance - before, 2 * PRICE_WEI);
        assertEq(address(ash).balance, 0);
    }

    // ---- ZEC path ------------------------------------------------------------

    function testZecMintHappyPath() public {
        uint256 rid = _reserve(alice); // a relayer reserves; alice holds no SOVA
        assertEq(co.quote(rid), PRICE_ZAT + 1);
        assertEq(co.payeeScript(rid), ZcashLib.p2pkh(PAYEE));
        assertEq(ash.zecHeld(), 1);
        bytes32 txid = _pay(rid);
        z.mine(MINCONF);
        uint256 id = _claim(rid, txid);
        assertEq(ash.ownerOf(id), alice);
        assertEq(ash.balanceOf(relayer), 0);
        assertEq(ash.zecHeld(), 0);
        assertEq(ash.totalSupply(), 1);
        assertTrue(bytes(ash.tokenURI(id)).length > 500);
        // The seed is keyed on the recipient, as a SOVA mint's is on the minter.
        assertTrue(ash.seedOf(id) != bytes32(0));
    }

    function testZecPaymentClaimableOnce() public {
        uint256 r1 = _reserve(alice);
        uint256 r2 = _reserve(bob);
        bytes32 txid = _pay(r1);
        z.mine(MINCONF);
        _claim(r1, txid);

        vm.expectRevert(ZecCheckout.AlreadyFilled.selector);
        _claim(r1, txid);
        vm.expectRevert(abi.encodeWithSelector(ZecCheckout.PaymentAlreadyUsed.selector, r1));
        _claim(r2, txid);
        // A second, different payment cannot fill r1 twice either.
        bytes32 again = keccak256("paid twice");
        z.pay(again, ZcashLib.p2pkh(PAYEE), co.quote(r1));
        z.mine(MINCONF);
        vm.expectRevert(ZecCheckout.AlreadyFilled.selector);
        _claim(r1, again);
        assertEq(ash.totalSupply(), 1);
        assertEq(ash.zecHeld(), 1); // r2 still holds its unit
    }

    function testZecWrongAmountAndLatePaymentsRefused() public {
        uint256 rid = _reserve(alice);
        bytes32 under = keccak256("under");
        z.pay(under, ZcashLib.p2pkh(PAYEE), PRICE_ZAT); // forgot the tag
        z.mine(MINCONF);
        vm.expectRevert(
            abi.encodeWithSelector(ZcashLib.ZcashUnderpaid.selector, under, uint32(0), PRICE_ZAT, PRICE_ZAT + 1)
        );
        _claim(rid, under);

        uint256 late = _reserve(bob);
        z.mine(WINDOW);
        bytes32 txid = _pay(late);
        z.mine(MINCONF);
        vm.expectRevert();
        _claim(late, txid);
    }

    function testTagsWrapInsteadOfRunningOut() public {
        bytes32 key = keccak256(abi.encode(false, PAYEE, PRICE_ZAT));
        stdstore.target(address(co)).sig(co.lastTag.selector).with_key(key).checked_write(uint256(co.TAG_SPACE() - 2));
        assertEq(co.quote(_reserve(alice)), PRICE_ZAT + co.TAG_SPACE() - 1); // last tag
        assertEq(co.quote(_reserve(bob)), PRICE_ZAT + 1); // wraps to 1
    }

    // ---- cap -----------------------------------------------------------------

    function testCapSovaOnly() public {
        _setSupply(9_999);
        uint256 id = _mint(alice);
        assertEq(id, 10_000);
        vm.prank(bob);
        vm.expectRevert(bytes("ASHW: sold out"));
        ash.mint{value: PRICE_WEI}();
        vm.expectRevert(bytes("ASHW: sold out"));
        _reserve(bob);
    }

    function testCapCountsZecHolds() public {
        _setSupply(9_998);
        uint256 rid = _reserve(alice); // holds unit 10,000
        _mint(bob); // unit 9,999
        vm.prank(carol);
        vm.expectRevert(bytes("ASHW: sold out"));
        ash.mint{value: PRICE_WEI}();
        vm.expectRevert(bytes("ASHW: sold out"));
        _reserve(carol);
        // The reserved buyer is never sold out from under.
        bytes32 txid = _pay(rid);
        z.mine(MINCONF);
        uint256 id = _claim(rid, txid);
        assertEq(id, 10_000);
        assertEq(ash.totalSupply(), ash.MAX_SUPPLY());
        assertEq(ash.zecHeld(), 0);
    }

    function testExpiredHoldFreesSupplyForSova() public {
        _setSupply(9_999);
        uint256 rid = _reserve(alice);
        bytes32 txid = _pay(rid); // paid in time...
        vm.prank(bob);
        vm.expectRevert(bytes("ASHW: sold out"));
        ash.mint{value: PRICE_WEI}();

        z.mine(co.holdBlocks() + 1); // ...but nobody claims before the hold ends
        _mint(bob); // the SOVA mint sweeps the expired hold and takes the unit
        assertEq(ash.totalSupply(), 10_000);
        assertEq(ash.zecHeld(), 0);
        assertEq(co.swept(), rid);
        vm.expectRevert(bytes("ASHW: sold out"));
        _claim(rid, txid);
    }

    function testExpiredHoldStillClaimableWhileSupplyLeft() public {
        uint256 rid = _reserve(alice);
        bytes32 txid = _pay(rid);
        z.mine(co.holdBlocks() + 5);
        assertEq(co.sweep(), 1);
        assertEq(ash.zecHeld(), 0);
        uint256 id = _claim(rid, txid);
        assertEq(ash.ownerOf(id), alice);
        assertEq(ash.zecHeld(), 0);
    }

    function testUnsweptExpiredHoldUsedByItsClaim() public {
        uint256 rid = _reserve(alice);
        bytes32 txid = _pay(rid);
        z.mine(co.holdBlocks() + 5); // expired, but nobody swept
        _claim(rid, txid);
        assertEq(ash.zecHeld(), 0);
        assertEq(co.sweep(), 0); // filled: nothing to release
        assertEq(co.swept(), rid);
    }

    function testSweepIsBounded() public {
        for (uint256 i = 0; i < 40; i++) {
            _reserve(alice);
        }
        assertEq(ash.zecHeld(), 40);
        z.mine(co.holdBlocks() + 1);
        assertEq(co.sweep(), 32);
        assertEq(ash.zecHeld(), 8);
        assertEq(co.sweep(), 8);
        assertEq(ash.zecHeld(), 0);
        assertEq(co.sweep(), 0);
    }

    function testHoldsExpireInOrder() public {
        uint256 r1 = _reserve(alice);
        z.mine(10);
        uint256 r2 = _reserve(bob);
        z.mine(co.holdBlocks() - 9); // r1 expired, r2 not
        assertEq(co.sweep(), 1);
        assertEq(co.swept(), r1);
        assertEq(ash.zecHeld(), 1);
        assertEq(co.holdUntil(r2), START + 10 + co.holdBlocks());
    }

    /// Fuzz the cap across both rails: the supply never passes 10,000 and
    /// holds + minted never do either.
    function testFuzzCapBothRails(uint8[24] calldata ops) public {
        _setSupply(9_990);
        uint256[] memory rids = new uint256[](ops.length);
        uint256 n;
        for (uint256 i = 0; i < ops.length; i++) {
            uint8 op = ops[i] % 4;
            if (op == 0) {
                vm.prank(alice);
                try ash.mint{value: PRICE_WEI}() {} catch {}
            } else if (op == 1) {
                vm.prank(relayer);
                try co.reserve(1, bob) returns (uint256 rid) {
                    rids[n++] = rid;
                } catch {}
            } else if (op == 2 && n > 0) {
                uint256 rid = rids[ops[i] % n];
                (,,,, bool filled,,,) = co.reservations(rid);
                if (!filled && z.anchorHeight() < co.deadline(rid)) {
                    bytes32 txid = _pay(rid);
                    z.mine(MINCONF);
                    vm.prank(relayer);
                    try co.claim(rid, txid, 0) {} catch {}
                }
            } else {
                z.mine(uint64(ops[i]));
            }
            assertLe(ash.totalSupply() + ash.zecHeld(), ash.MAX_SUPPLY());
        }
    }

    // ---- gas -----------------------------------------------------------------

    function testGasMintPaths() public {
        _mint(carol); // warm totalSupply like a live collection
        vm.cool(address(ash));
        uint256 g = gasleft();
        vm.prank(alice);
        ash.mint{value: PRICE_WEI}();
        uint256 mintGas = g - gasleft();

        vm.cool(address(ash));
        vm.cool(address(co));
        vm.cool(ZCASH_PRECOMPILE);
        g = gasleft();
        vm.prank(relayer);
        uint256 rid = co.reserve(1, bob);
        uint256 reserveGas = g - gasleft();

        bytes32 txid = _pay(rid);
        z.mine(MINCONF);
        vm.cool(address(ash));
        vm.cool(address(co));
        vm.cool(ZCASH_PRECOMPILE);
        g = gasleft();
        vm.prank(relayer);
        co.claim(rid, txid, 0);
        uint256 claimGas = g - gasleft();

        console2.log("mint (SOVA) gas, excl. 21k base", mintGas);
        console2.log("reserve (ZEC) gas, mock anchor", reserveGas);
        console2.log("claim (ZEC) gas, mock precompile", claimGas);
        assertLt(mintGas, 120_000);
        assertLt(claimGas, 300_000);
    }
}

// ---------------------------------------------------------------------------
// Market
// ---------------------------------------------------------------------------

/// A seller whose payment hook tries to re-enter the market.
contract ReentrantSeller {
    Ashwings immutable ash;
    AshwingsMarket immutable market;
    uint256 public targetId;
    bytes public reentryError;
    bool public reentered;
    bool public tried;
    uint8 public mode; // 0 buy, 1 cancel, 2 list, 3 withdrawFees

    constructor(Ashwings a, AshwingsMarket m) {
        ash = a;
        market = m;
    }

    function mintAndList(uint256 price) external payable returns (uint256 id) {
        id = ash.mint{value: msg.value}();
        ash.approve(address(market), id);
        market.list(id, price);
    }

    function arm(uint256 id, uint8 m) external {
        targetId = id;
        mode = m;
    }

    receive() external payable {
        if (tried) return;
        tried = true;
        bytes memory call_ = mode == 0
            ? abi.encodeCall(market.buy, (targetId))
            : mode == 1
                ? abi.encodeCall(market.cancel, (targetId))
                : mode == 2 ? abi.encodeCall(market.list, (targetId, 1 ether)) : abi.encodeCall(market.withdrawFees, ());
        (bool ok, bytes memory err) = address(market).call{value: mode == 0 ? msg.value : 0}(call_);
        reentered = ok;
        reentryError = err;
    }
}

/// A treasury that refuses SOVA.
contract RefusingTreasury {
    receive() external payable {
        revert("no");
    }
}

contract AshwingsMarketTest is AshwingsFixture {
    uint256 constant LIST = 25 ether;

    function _mintApproveList(address who, uint256 price) internal returns (uint256 id) {
        id = _mint(who);
        vm.startPrank(who);
        ash.approve(address(market), id);
        market.list(id, price);
        vm.stopPrank();
    }

    function testTreasuryAndFeeAreFixed() public {
        assertEq(market.treasury(), TREASURY);
        assertEq(market.ashwings(), address(ash));
        assertEq(market.feeBps(), 100);
        vm.expectRevert(bytes("MARKET: bad params"));
        new AshwingsMarket(address(ash), TREASURY, 1_001);
        vm.expectRevert(bytes("MARKET: bad params"));
        new AshwingsMarket(address(ash), address(0), 100);
        AshwingsMarket m250 = new AshwingsMarket(address(ash), TREASURY, 250);
        assertEq(m250.feeOf(1 ether), 0.025 ether);
        assertEq(m250.feeOf(39), 0); // floor(39 * 2.5%) = 0
    }

    function testListBuyExactOnePercent() public {
        uint256 id = _mint(alice);
        vm.startPrank(alice);
        ash.approve(address(market), id);
        vm.expectEmit(address(market));
        emit AshwingsMarket.Listed(id, alice, LIST);
        market.list(id, LIST);
        vm.stopPrank();
        assertTrue(market.isLive(id));

        uint256 aliceBefore = alice.balance;
        vm.expectEmit(address(market));
        emit AshwingsMarket.Sold(id, alice, bob, LIST, 0.25 ether);
        vm.prank(bob);
        market.buy{value: LIST}(id);

        assertEq(ash.ownerOf(id), bob);
        assertEq(alice.balance - aliceBefore, 24.75 ether);
        assertEq(market.feesOwed(), 0.25 ether);
        assertEq(address(market).balance, 0.25 ether);
        (address seller, uint96 price) = market.listings(id);
        assertEq(seller, address(0));
        assertEq(price, 0);

        uint256 tBefore = TREASURY.balance;
        vm.prank(carol);
        market.withdrawFees();
        assertEq(TREASURY.balance - tBefore, 0.25 ether);
        assertEq(address(market).balance, 0);
    }

    /// fee = floor(price / 100); seller gets the rest; nothing is lost.
    function testFeeRounding() public view {
        assertEq(market.feeOf(99), 0); // under 100 wei: no fee
        assertEq(market.feeOf(100), 1);
        assertEq(market.feeOf(199), 1); // rounds down, seller keeps 198
        assertEq(market.feeOf(1 ether), 0.01 ether);
    }

    function testFuzzFeeMath(uint96 price) public {
        vm.assume(price > 0 && price < 500 ether);
        uint256 id = _mintApproveList(alice, price);
        uint256 aliceBefore = alice.balance;
        vm.deal(bob, price);
        vm.prank(bob);
        market.buy{value: price}(id);
        uint256 fee = uint256(price) / 100;
        assertEq(market.feesOwed(), fee);
        assertEq(alice.balance - aliceBefore, price - fee);
        assertEq(fee + (alice.balance - aliceBefore), price);
        assertEq(address(market).balance, fee);
    }

    function testBuyNeedsExactValue() public {
        uint256 id = _mintApproveList(alice, LIST);
        vm.startPrank(bob);
        vm.expectRevert(abi.encodeWithSelector(AshwingsMarket.WrongValue.selector, LIST - 1, LIST));
        market.buy{value: LIST - 1}(id);
        vm.expectRevert(abi.encodeWithSelector(AshwingsMarket.WrongValue.selector, LIST + 1, LIST));
        market.buy{value: LIST + 1}(id);
        vm.stopPrank();
    }

    function testListRequiresOwnerAndTokenApproval() public {
        uint256 id = _mint(alice);
        vm.prank(bob);
        vm.expectRevert(AshwingsMarket.NotOwner.selector);
        market.list(id, LIST);
        vm.prank(alice);
        vm.expectRevert(AshwingsMarket.NotApproved.selector);
        market.list(id, LIST);
        // Operator approval survives transfers, so it is not enough.
        vm.startPrank(alice);
        ash.setApprovalForAll(address(market), true);
        vm.expectRevert(AshwingsMarket.NotApproved.selector);
        market.list(id, LIST);
        ash.approve(address(market), id);
        vm.expectRevert(AshwingsMarket.BadPrice.selector);
        market.list(id, 0);
        vm.expectRevert(AshwingsMarket.BadPrice.selector);
        market.list(id, uint256(type(uint96).max) + 1);
        vm.expectRevert(AshwingsMarket.NotOwner.selector);
        market.list(999, LIST); // unminted
        vm.stopPrank();
    }

    function testRelistReplacesPrice() public {
        uint256 id = _mintApproveList(alice, LIST);
        vm.prank(alice);
        market.list(id, 2 * LIST);
        vm.prank(bob);
        vm.expectRevert(abi.encodeWithSelector(AshwingsMarket.WrongValue.selector, LIST, 2 * LIST));
        market.buy{value: LIST}(id);
        vm.prank(bob);
        market.buy{value: 2 * LIST}(id);
        assertEq(ash.ownerOf(id), bob);
    }

    function testCancel() public {
        uint256 id = _mintApproveList(alice, LIST);
        vm.prank(bob);
        vm.expectRevert(AshwingsMarket.NotSeller.selector);
        market.cancel(id);
        vm.expectEmit(address(market));
        emit AshwingsMarket.Canceled(id, alice);
        vm.prank(alice);
        market.cancel(id);
        assertFalse(market.isLive(id));
        vm.prank(bob);
        vm.expectRevert(AshwingsMarket.NotListed.selector);
        market.buy{value: LIST}(id);
        vm.prank(alice);
        vm.expectRevert(AshwingsMarket.NotListed.selector);
        market.cancel(id);
    }

    function testStaleWhenOwlMoves() public {
        uint256 id = _mintApproveList(alice, LIST);
        vm.prank(alice);
        ash.transferFrom(alice, carol, id);
        assertFalse(market.isLive(id));
        vm.prank(bob);
        vm.expectRevert(AshwingsMarket.StaleListing.selector);
        market.buy{value: LIST}(id);

        // Coming back does not revive it: the transfer cleared the approval.
        vm.prank(carol);
        ash.transferFrom(carol, alice, id);
        assertFalse(market.isLive(id));
        vm.prank(bob);
        vm.expectRevert(AshwingsMarket.StaleListing.selector);
        market.buy{value: LIST}(id);

        // Anyone may clear a stale listing.
        vm.prank(bob);
        market.cancel(id);
        (address seller,) = market.listings(id);
        assertEq(seller, address(0));
    }

    function testStaleWhenApprovalRevoked() public {
        uint256 id = _mintApproveList(alice, LIST);
        vm.prank(alice);
        ash.approve(address(0), id);
        assertFalse(market.isLive(id));
        vm.prank(bob);
        vm.expectRevert(AshwingsMarket.StaleListing.selector);
        market.buy{value: LIST}(id);
        // Re-approving is the seller's explicit choice: the listing lives again.
        vm.prank(alice);
        ash.approve(address(market), id);
        assertTrue(market.isLive(id));
    }

    function testStaleWhenApprovalGivenToSomeoneElse() public {
        uint256 id = _mintApproveList(alice, LIST);
        vm.prank(alice);
        ash.approve(carol, id);
        vm.prank(bob);
        vm.expectRevert(AshwingsMarket.StaleListing.selector);
        market.buy{value: LIST}(id);
    }

    function testReentrantSellerCannotReenter() public {
        for (uint8 mode = 0; mode < 4; mode++) {
            ReentrantSeller evil = new ReentrantSeller(ash, market);
            vm.deal(address(evil), 0);
            uint256 id = evil.mintAndList{value: PRICE_WEI}(LIST);
            uint256 other = _mintApproveList(alice, LIST); // a second, honest listing
            evil.arm(mode == 0 ? other : id, mode);

            vm.prank(bob);
            market.buy{value: LIST}(id); // pays evil, whose receive() tries to re-enter
            assertTrue(evil.tried());
            assertFalse(evil.reentered());
            assertEq(bytes4(evil.reentryError()), AshwingsMarket.Reentrancy.selector);
            // Nothing moved except the one sale.
            assertEq(ash.ownerOf(id), bob);
            assertEq(ash.ownerOf(other), alice);
            assertTrue(market.isLive(other));
            assertEq(address(evil).balance, LIST - LIST / 100);
            vm.prank(alice);
            market.cancel(other);
        }
    }

    function testRefusingTreasuryNeverBlocksSales() public {
        RefusingTreasury t = new RefusingTreasury();
        Ashwings a2 = new Ashwings(address(t), payeeT, PRICE_WEI, PRICE_ZAT);
        AshwingsMarket m2 = new AshwingsMarket(address(a2), address(t), 100);
        vm.startPrank(alice);
        uint256 id = a2.mint{value: PRICE_WEI}();
        a2.approve(address(m2), id);
        m2.list(id, LIST);
        vm.stopPrank();
        vm.prank(bob);
        m2.buy{value: LIST}(id);
        assertEq(a2.ownerOf(id), bob);
        vm.expectRevert(AshwingsMarket.PaymentFailed.selector);
        m2.withdrawFees();
        assertEq(m2.feesOwed(), LIST / 100); // still booked
        vm.expectRevert(bytes("ASHW: withdraw failed"));
        a2.withdraw();
    }

    function testGasMarket() public {
        uint256 id = _mint(alice);
        _mintApproveList(carol, LIST); // warm feesOwed-free state like a live market
        vm.prank(alice);
        ash.approve(address(market), id);
        vm.cool(address(ash));
        vm.cool(address(market));
        uint256 g = gasleft();
        vm.prank(alice);
        market.list(id, LIST);
        uint256 listGas = g - gasleft();

        vm.cool(address(ash));
        vm.cool(address(market));
        g = gasleft();
        vm.prank(bob);
        market.buy{value: LIST}(id);
        uint256 buyGas = g - gasleft();

        _mintApproveList(alice, LIST);
        vm.cool(address(market));
        vm.cool(address(ash));
        g = gasleft();
        vm.prank(alice);
        market.cancel(3);
        uint256 cancelGas = g - gasleft();

        console2.log("list gas, excl. 21k base", listGas);
        console2.log("buy gas, excl. 21k base", buyGas);
        console2.log("cancel gas, excl. 21k base", cancelGas);
    }
}
