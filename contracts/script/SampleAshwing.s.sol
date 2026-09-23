// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {Ashwings} from "../src/Ashwings.sol";

contract SampleAshwing is Script {
    function run() external {
        Ashwings r = new Ashwings();
        for (uint256 i = 0; i < 4; i++) {
            vm.prank(address(uint160(0xA11CE + i)));
            uint256 id = r.mint();
            vm.roll(block.number + 1);
            vm.prevrandao(keccak256(abi.encode(i, block.number)));
            vm.writeFile(string.concat("sample-ashwing-", vm.toString(id), ".txt"), r.tokenURI(id));
        }
    }
}
