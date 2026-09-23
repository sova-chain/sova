// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {WSOVA} from "../src/WSOVA.sol";

interface IFactory {
    function createPair(address, address) external returns (address);
    function getPair(address, address) external view returns (address);
}

interface IRouter {
    function addLiquidityETH(address token, uint256 amountTokenDesired, uint256 amountTokenMin, uint256 amountETHMin, address to, uint256 deadline)
        external payable returns (uint256, uint256, uint256);
    function swapExactETHForTokens(uint256 amountOutMin, address[] calldata path, address to, uint256 deadline)
        external payable returns (uint256[] memory);
    function swapExactTokensForETH(uint256 amountIn, uint256 amountOutMin, address[] calldata path, address to, uint256 deadline)
        external returns (uint256[] memory);
    function WETH() external view returns (address);
}

interface IERC20Like {
    function approve(address, uint256) external returns (bool);
    function balanceOf(address) external view returns (uint256);
    function transfer(address, uint256) external returns (bool);
}

contract TestToken {
    string public constant name = "Day One Token";
    string public constant symbol = "DAY1";
    uint8 public constant decimals = 18;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor(uint256 supply) {
        totalSupply = supply;
        balanceOf[msg.sender] = supply;
        emit Transfer(address(0), msg.sender, supply);
    }

    function approve(address spender, uint256 value) external returns (bool) {
        allowance[msg.sender][spender] = value;
        emit Approval(msg.sender, spender, value);
        return true;
    }

    function transfer(address to, uint256 value) external returns (bool) {
        return _move(msg.sender, to, value);
    }

    function transferFrom(address from, address to, uint256 value) external returns (bool) {
        if (from != msg.sender && allowance[from][msg.sender] != type(uint256).max) {
            require(allowance[from][msg.sender] >= value, "DAY1: allowance");
            allowance[from][msg.sender] -= value;
        }
        return _move(from, to, value);
    }

    function _move(address from, address to, uint256 value) internal returns (bool) {
        require(balanceOf[from] >= value, "DAY1: balance");
        balanceOf[from] -= value;
        balanceOf[to] += value;
        emit Transfer(from, to, value);
        return true;
    }
}

/// The day-one loop, end to end on the AMM fork: wrap-capable router,
/// launch a token, seed a native-SOVA pool, swap in, swap out.
contract AmmTest is Test {
    WSOVA wsova;
    address factory;
    address router;
    TestToken token;
    address alice = address(0xA11CE);

    function setUp() public {
        wsova = new WSOVA();
        factory = deployCode("UniswapV2Factory.sol:UniswapV2Factory", abi.encode(address(0)));
        router = deployCode(
            "UniswapV2Router02.sol:UniswapV2Router02", abi.encode(factory, address(wsova))
        );
        vm.deal(alice, 1_000 ether);
        vm.prank(alice);
        token = new TestToken(1_000_000 ether);
    }

    function testRouterKnowsWsova() public view {
        assertEq(IRouter(router).WETH(), address(wsova));
    }

    function testLaunchPoolSwapRoundTrip() public {
        vm.startPrank(alice);
        token.approve(router, type(uint256).max);

        // Seed: 100 SOVA / 10,000 DAY1.
        IRouter(router).addLiquidityETH{value: 100 ether}(
            address(token), 10_000 ether, 0, 0, alice, block.timestamp + 1
        );
        address pair = IFactory(factory).getPair(address(wsova), address(token));
        assertTrue(pair != address(0), "pair exists");
        assertGt(IERC20Like(pair).balanceOf(alice), 0, "LP tokens minted");

        // Swap 1 SOVA -> DAY1.
        address[] memory path = new address[](2);
        path[0] = address(wsova);
        path[1] = address(token);
        uint256 before = token.balanceOf(alice);
        IRouter(router).swapExactETHForTokens{value: 1 ether}(0, path, alice, block.timestamp + 1);
        uint256 got = token.balanceOf(alice) - before;
        // ~99 DAY1 minus 0.3% fee and price impact.
        assertGt(got, 95 ether);
        assertLt(got, 100 ether);

        // Swap DAY1 back -> native SOVA.
        path[0] = address(token);
        path[1] = address(wsova);
        uint256 sovaBefore = alice.balance;
        IRouter(router).swapExactTokensForETH(got, 0, path, alice, block.timestamp + 1);
        assertGt(alice.balance, sovaBefore, "native SOVA received");
        vm.stopPrank();
    }
}
