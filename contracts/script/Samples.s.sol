// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {Ashwings} from "../src/Ashwings.sol";

/// Renders the gallery samples with the contract itself: for each seed in
/// samples/seeds.txt (sample seeds keccak256(uint256(i)) picked for
/// variety), mint a token, plant the seed (seedOf is storage slot 5) and
/// write svgOf(id) to samples/onchain-N.svg, N = line number from 0.
/// Local simulation only, no RPC:
///   forge script script/Samples.s.sol --tc Samples
/// The site copies these into site/public/ashwings/.
contract Samples is Script {
    function run() external {
        Ashwings r = new Ashwings();
        for (uint256 n = 0;; n++) {
            string memory line = vm.readLine("samples/seeds.txt");
            if (bytes(line).length == 0) break;
            uint256 id = r.mint();
            vm.store(address(r), keccak256(abi.encode(id, uint256(5))), vm.parseBytes32(line));
            vm.writeFile(string.concat("samples/onchain-", vm.toString(n), ".svg"), r.svgOf(id));
        }
    }
}
