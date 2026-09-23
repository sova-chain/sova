// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console} from "forge-std/Script.sol";

contract InitHash is Script {
    function run() external view {
        bytes memory code = vm.getCode("UniswapV2Pair.sol:UniswapV2Pair");
        console.logBytes32(keccak256(code));
    }
}
