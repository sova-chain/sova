// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {WSOVA} from "../src/WSOVA.sol";
import {Ashwings, IERC721Receiver} from "../src/Ashwings.sol";

contract WSOVATest is Test {
    WSOVA w;
    address alice = address(0xA11CE);
    address bob = address(0xB0B);

    function setUp() public {
        w = new WSOVA();
        vm.deal(alice, 100 ether);
    }

    function testDepositWithdrawRoundTrip() public {
        vm.startPrank(alice);
        w.deposit{value: 5 ether}();
        assertEq(w.balanceOf(alice), 5 ether);
        assertEq(w.totalSupply(), 5 ether);
        w.withdraw(2 ether);
        assertEq(w.balanceOf(alice), 3 ether);
        assertEq(alice.balance, 97 ether);
        vm.stopPrank();
    }

    function testReceiveFallbackDeposits() public {
        vm.prank(alice);
        (bool ok,) = address(w).call{value: 1 ether}("");
        assertTrue(ok);
        assertEq(w.balanceOf(alice), 1 ether);
    }

    function testTransferAndAllowance() public {
        vm.startPrank(alice);
        w.deposit{value: 4 ether}();
        w.transfer(bob, 1 ether);
        w.approve(bob, 2 ether);
        vm.stopPrank();
        vm.prank(bob);
        w.transferFrom(alice, bob, 2 ether);
        assertEq(w.balanceOf(bob), 3 ether);
        assertEq(w.allowance(alice, bob), 0);
    }

    function testWithdrawInsufficientReverts() public {
        vm.prank(alice);
        vm.expectRevert(bytes("WSOVA: insufficient"));
        w.withdraw(1);
    }
}

contract AshwingsTest is Test {
    Ashwings r;
    address alice = address(0xA11CE);
    address bob = address(0xB0B);

    function setUp() public {
        // Free SOVA price (0) so mint() is the plain call; the paid paths
        // are covered in AshwingsV2.t.sol.
        r = new Ashwings(address(0x7EA5), "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV", 0, 25_000_000);
    }

    function testMintAssignsOwnershipAndSeed() public {
        vm.prank(alice);
        uint256 id = r.mint();
        assertEq(id, 1);
        assertEq(r.ownerOf(1), alice);
        assertEq(r.balanceOf(alice), 1);
        assertTrue(r.seedOf(1) != bytes32(0));
    }

    function testSeedsDifferAcrossMints() public {
        vm.prank(alice);
        uint256 a = r.mint();
        vm.prank(bob);
        uint256 b = r.mint();
        assertTrue(r.seedOf(a) != r.seedOf(b));
    }

    function testTokenURIIsOnChainDataURI() public {
        vm.prank(alice);
        uint256 id = r.mint();
        string memory uri = r.tokenURI(id);
        assertEq(_prefix(uri, 29), "data:application/json;base64,");
        // The payload must be non-trivial (a full JSON + embedded SVG).
        assertGt(bytes(uri).length, 500);
    }

    function testCollectionNameAndSymbol() public view {
        assertEq(r.name(), "Ashwings");
        assertEq(r.symbol(), "ASHW");
    }

    function testTokenURINamesTheAshwing() public {
        vm.prank(alice);
        uint256 id = r.mint();
        string memory uri = r.tokenURI(id);
        // '{"name":"Ashwing #' is 18 bytes (a multiple of 3), so its base64
        // is exactly the first 24 chars of the payload after the prefix.
        string memory want = vm.toBase64(bytes('{"name":"Ashwing #'));
        bytes memory u = bytes(uri);
        bytes memory got = new bytes(bytes(want).length);
        for (uint256 i = 0; i < got.length; i++) {
            got[i] = u[29 + i];
        }
        assertEq(string(got), want);
    }

    function testTokenURIUnmintedReverts() public {
        vm.expectRevert(bytes("ASHW: unminted"));
        r.tokenURI(999);
    }

    function testTransferFlow() public {
        vm.prank(alice);
        uint256 id = r.mint();
        vm.prank(alice);
        r.transferFrom(alice, bob, id);
        assertEq(r.ownerOf(id), bob);
        assertEq(r.balanceOf(alice), 0);
        assertEq(r.balanceOf(bob), 1);
    }

    function testUnauthorizedTransferReverts() public {
        vm.prank(alice);
        uint256 id = r.mint();
        vm.prank(bob);
        vm.expectRevert(bytes("ASHW: not authorized"));
        r.transferFrom(alice, bob, id);
    }

    function testApproveThenTransfer() public {
        vm.prank(alice);
        uint256 id = r.mint();
        vm.prank(alice);
        r.approve(bob, id);
        vm.prank(bob);
        r.transferFrom(alice, bob, id);
        assertEq(r.ownerOf(id), bob);
    }

    function testSafeTransferToNonReceiverContractReverts() public {
        vm.prank(alice);
        uint256 id = r.mint();
        // WSOVA implements no onERC721Received; its receive() accepts
        // plain value but the ERC-721 safety check must still reject it.
        WSOVA notAReceiver = new WSOVA();
        vm.prank(alice);
        vm.expectRevert();
        r.safeTransferFrom(alice, address(notAReceiver), id);
    }

    function _prefix(string memory s, uint256 n) internal pure returns (string memory) {
        bytes memory b = bytes(s);
        bytes memory out = new bytes(n);
        for (uint256 i = 0; i < n; i++) {
            out[i] = b[i];
        }
        return string(out);
    }
}
