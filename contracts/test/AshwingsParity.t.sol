// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, console} from "forge-std/Test.sol";
import {Ashwings} from "../src/Ashwings.sol";

/// Art parity: the contract's SVG and metadata must be byte-for-byte what
/// the design lab generator (contracts/design/ashwing24.py) produces.
///
/// Golden files in test/fixtures/ashwings24/ (one line per seed, in
/// lockstep: seeds.txt, svgs.txt, attrs.txt; labels.txt says why each
/// seed is there) are written by
///   python3 design/ashwing24.py --fixtures test/fixtures/ashwings24
/// and read here with vm.readLine: no FFI. design/check-parity.sh
/// regenerates them into a temp dir, diffs, then runs this test.
///
/// Each seed is planted into a real minted token (seedOf is storage slot
/// 5), so the public svgOf/tokenURI paths are what gets checked.
contract AshwingsParityTest is Test {
    Ashwings r;
    string constant DIR = "test/fixtures/ashwings24/";
    string constant DESC =
        "A fully on-chain owl, minted with destroyed money: every wei of SOVA gas traces back to burned ZEC.";
    uint256 constant SEED_SLOT = 5;
    // Searched worst case (most rects: 234 incl. background), see
    // WORST_SEED in design/ashwing24.py.
    bytes32 constant WORST = 0x0000be002600f6d000aa000000000fffffffffffffffffffffffffffffffffff;

    function setUp() public {
        // Free SOVA price (0) so mint() is the plain call; the paid paths
        // are covered in AshwingsV2.t.sol.
        r = new Ashwings(address(0x7EA5), "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV", 0, 25_000_000);
    }

    function _mintWithSeed(bytes32 seed) internal returns (uint256 id) {
        vm.prank(address(0xA11CE));
        id = r.mint();
        vm.store(address(r), keccak256(abi.encode(id, SEED_SLOT)), seed);
        assertEq(r.seedOf(id), seed, "seed slot");
    }

    function _expectedURI(uint256 id, string memory attrs, string memory svg) internal pure returns (string memory) {
        string memory json = string.concat(
            '{"name":"Ashwing #',
            vm.toString(id),
            '","description":"',
            DESC,
            '","attributes":',
            attrs,
            ',"image":"data:image/svg+xml;base64,',
            vm.toBase64(bytes(svg)),
            '"}'
        );
        return string.concat("data:application/json;base64,", vm.toBase64(bytes(json)));
    }

    function testGoldenParity() public {
        uint256 n;
        while (true) {
            string memory seedLine = vm.readLine(string.concat(DIR, "seeds.txt"));
            if (bytes(seedLine).length == 0) break;
            string memory svg = vm.readLine(string.concat(DIR, "svgs.txt"));
            string memory attrs = vm.readLine(string.concat(DIR, "attrs.txt"));
            string memory label = vm.readLine(string.concat(DIR, "labels.txt"));
            // A fresh call frame per seed keeps memory (and so gas) flat.
            this.checkSeed(seedLine, svg, attrs, label);
            n++;
        }
        assertGe(n, 64, "too few golden seeds");
        console.log("golden seeds byte-identical (svgOf + tokenURI):", n);
    }

    function checkSeed(string calldata seedLine, string calldata svg, string calldata attrs, string calldata label)
        external
    {
        uint256 id = _mintWithSeed(vm.parseBytes32(seedLine));
        assertEq(r.svgOf(id), svg, string.concat("svg mismatch: ", seedLine, " ", label));
        assertEq(
            r.tokenURI(id), _expectedURI(id, attrs, svg), string.concat("tokenURI mismatch: ", seedLine, " ", label)
        );
    }

    function testWorstCaseRenderGas() public {
        uint256 id = _mintWithSeed(WORST);
        uint256 g0 = gasleft();
        string memory svg = r.svgOf(id);
        uint256 svgGas = g0 - gasleft();
        g0 = gasleft();
        r.tokenURI(id);
        uint256 uriGas = g0 - gasleft();
        // 234 rects = 1 background + 233 run rects.
        assertEq(_count(bytes(svg), "<rect "), 234, "worst-case rect count");
        console.log("worst-case svgOf gas:", svgGas);
        console.log("worst-case tokenURI gas:", uriGas);
        assertLt(uriGas, 30_000_000, "tokenURI must fit a 30M eth_call");
        assertLt(uriGas, 10_000_000, "tokenURI render budget");
    }

    function testTraitsOfMatchesSeedBytes() public {
        // bytes 0..9 all 0xFF: the last value of every table.
        uint256 id = _mintWithSeed(bytes32(type(uint256).max));
        Ashwings.Traits memory t = r.traitsOf(id);
        assertEq(t.species, 10);
        assertEq(t.background, 7);
        assertEq(t.tufts, 5);
        assertEq(t.eyes, 3);
        assertEq(t.dx, 1);
        assertEq(t.pupil, 1);
        assertEq(t.iris, 3);
        assertEq(t.beak, 3);
        assertEq(t.chest, 3);
        assertEq(t.accessory, 7);
        // All zero: the first value of every table (glance index 0 = left).
        id = _mintWithSeed(bytes32(0));
        t = r.traitsOf(id);
        assertEq(t.species, 0);
        assertEq(t.dx, -1);
        assertEq(t.accessory, 0);
    }

    function testSvgOfUnmintedReverts() public {
        vm.expectRevert(bytes("ASHW: unminted"));
        r.svgOf(1);
    }

    function _count(bytes memory hay, bytes memory needle) internal pure returns (uint256 c) {
        for (uint256 i = 0; i + needle.length <= hay.length; i++) {
            bool m = true;
            for (uint256 j = 0; j < needle.length; j++) {
                if (hay[i + j] != needle[j]) {
                    m = false;
                    break;
                }
            }
            if (m) c++;
        }
    }
}
