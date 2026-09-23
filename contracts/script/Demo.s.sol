// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console} from "forge-std/Script.sol";
import {Ashwings} from "../src/Ashwings.sol";

interface IRouterD {
    function addLiquidityETH(address, uint256, uint256, uint256, address, uint256)
        external payable returns (uint256, uint256, uint256);
    function swapExactETHForTokens(uint256, address[] calldata, address, uint256)
        external payable returns (uint256[] memory);
}

contract DemoToken {
    string public name;
    string public symbol;
    uint8 public constant decimals = 18;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor(string memory n, string memory s, uint256 supply) {
        (name, symbol, totalSupply) = (n, s, supply);
        balanceOf[msg.sender] = supply;
        emit Transfer(address(0), msg.sender, supply);
    }

    function approve(address spender, uint256 value) external returns (bool) {
        allowance[msg.sender][spender] = value;
        emit Approval(msg.sender, spender, value);
        return true;
    }

    function transfer(address to, uint256 value) external returns (bool) {
        return transferFrom(msg.sender, to, value);
    }

    function transferFrom(address from, address to, uint256 value) public returns (bool) {
        if (from != msg.sender && allowance[from][msg.sender] != type(uint256).max) {
            require(allowance[from][msg.sender] >= value, "DEMO: allowance");
            allowance[from][msg.sender] -= value;
        }
        require(balanceOf[from] >= value, "DEMO: balance");
        balanceOf[from] -= value;
        balanceOf[to] += value;
        emit Transfer(from, to, value);
        return true;
    }
}

/// The day-one loop, live: launch a token, seed a native-SOVA pool,
/// swap, mint an owl. Reads router/ashwings from deployments.json.
contract Demo is Script {
    function run() external {
        string memory json = vm.readFile("deployments.json");
        address router = vm.parseJsonAddress(json, ".router");
        address ashwings = vm.parseJsonAddress(json, ".ashwings");

        vm.startBroadcast();
        DemoToken token = new DemoToken("Day One", "DAY1", 1_000_000 ether);
        token.approve(router, type(uint256).max);
        IRouterD(router).addLiquidityETH{value: 10 ether}(
            address(token), 100_000 ether, 0, 0, msg.sender, block.timestamp + 300
        );
        address[] memory path = new address[](2);
        path[0] = vm.parseJsonAddress(json, ".wsova");
        path[1] = address(token);
        IRouterD(router).swapExactETHForTokens{value: 1 ether}(
            0, path, msg.sender, block.timestamp + 300
        );
        uint256 ashwingId = Ashwings(ashwings).mint();
        vm.stopBroadcast();

        console.log("DAY1 token:", address(token));
        console.log("DAY1 balance after swap:", token.balanceOf(msg.sender) / 1 ether);
        console.log("Ashwing minted, id:", ashwingId);
        // NOTE: this species log comes from forge's SIMULATION pass;
        // the real broadcast lands in a different block, so the actual
        // seed (prevrandao/blockhash) differs. Read the truth with:
        //   cast call $ASHWINGS "tokenURI(uint256)(string)" <id>
        console.log("Ashwing species (simulated):", uint256(Ashwings(ashwings).traitsOf(ashwingId).species));
    }
}
