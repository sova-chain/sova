// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console} from "forge-std/Script.sol";
import {WSOVA} from "../src/WSOVA.sol";
import {Ashwings} from "../src/Ashwings.sol";

/// Day-one kit deploy: WSOVA, UniV2 factory+router, Multicall3, Ashwings.
/// No owners anywhere: the factory's feeToSetter is the zero address —
/// nobody can ever switch the protocol fee on.
contract Deploy is Script {
    function run() external {
        vm.startBroadcast();
        WSOVA wsova = new WSOVA();
        address factory = deployCode("UniswapV2Factory.sol:UniswapV2Factory", abi.encode(address(0)));
        address router =
            deployCode("UniswapV2Router02.sol:UniswapV2Router02", abi.encode(factory, address(wsova)));
        address multicall3 = deployCode("Multicall3.sol:Multicall3");
        Ashwings ashwings = new Ashwings();
        vm.stopBroadcast();

        console.log("WSOVA:     ", address(wsova));
        console.log("Factory:   ", factory);
        console.log("Router:    ", router);
        console.log("Multicall3:", multicall3);
        console.log("Ashwings:  ", address(ashwings));

        string memory json = "deploy";
        vm.serializeAddress(json, "wsova", address(wsova));
        vm.serializeAddress(json, "factory", factory);
        vm.serializeAddress(json, "router", router);
        vm.serializeAddress(json, "multicall3", multicall3);
        string memory out = vm.serializeAddress(json, "ashwings", address(ashwings));
        vm.writeJson(out, "deployments.json");
    }
}
