// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console} from "forge-std/Script.sol";
import {Ashwings} from "../src/Ashwings.sol";
import {AshwingsMarket} from "../src/AshwingsMarket.sol";

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
/// swap, mint an owl for SOVA, list it on the market, and a second account
/// buys it (1% to the treasury); then sweep mint income and fees to the
/// treasury. Reads addresses from deployments.json. The buyer is
/// SOVA_DEMO_BUYER_KEY (default: dev account #1), funded here by the
/// deployer. The ZEC path needs the SIP-4 precompile, which forge's local
/// simulation cannot call; box/deploy-dapps.sh reserves a ZEC order with
/// cast instead.
contract Demo is Script {
    uint256 constant DEV_KEY_1 = 0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d;
    uint256 constant LIST_PRICE = 25 ether;

    function run() external {
        string memory json = vm.readFile("deployments.json");
        address router = vm.parseJsonAddress(json, ".router");
        Ashwings ashwings = Ashwings(vm.parseJsonAddress(json, ".ashwings"));
        AshwingsMarket market = AshwingsMarket(vm.parseJsonAddress(json, ".market"));
        uint256 buyerKey = vm.envOr("SOVA_DEMO_BUYER_KEY", DEV_KEY_1);
        address buyer = vm.addr(buyerKey);
        address treasury = ashwings.treasury();
        uint256 treasuryBefore = treasury.balance;

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
        uint256 ashwingId = ashwings.mint{value: ashwings.priceWei()}();
        ashwings.approve(address(market), ashwingId);
        market.list(ashwingId, LIST_PRICE);
        (bool funded,) = buyer.call{value: LIST_PRICE + 1 ether}("");
        require(funded, "fund buyer");
        vm.stopBroadcast();

        vm.startBroadcast(buyerKey);
        market.buy{value: LIST_PRICE}(ashwingId);
        vm.stopBroadcast();

        vm.startBroadcast();
        ashwings.withdraw();
        market.withdrawFees();
        vm.stopBroadcast();

        console.log("DAY1 token:", address(token));
        console.log("DAY1 balance after swap:", token.balanceOf(msg.sender) / 1 ether);
        console.log("Ashwing minted for SOVA (wei):", ashwingId, ashwings.priceWei());
        console.log("listed at 25 SOVA and bought by", buyer);
        console.log("owner now:", ashwings.ownerOf(ashwingId));
        console.log("market fee (wei, 1%):", market.feeOf(LIST_PRICE));
        console.log("treasury received (wei):", treasury.balance - treasuryBefore);
        console.log("supply:", ashwings.totalSupply(), "/", ashwings.MAX_SUPPLY());
        // NOTE: this species log comes from forge's SIMULATION pass;
        // the real broadcast lands in a different block, so the actual
        // seed (prevrandao/blockhash) differs. Read the truth with:
        //   cast call $ASHWINGS "tokenURI(uint256)(string)" <id>
        console.log("Ashwing species (simulated):", uint256(ashwings.traitsOf(ashwingId).species));
    }
}
