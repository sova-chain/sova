// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

//  ###################################################################
//  ##  NEEDS SIP-4 LIVE. Every step except `tAddr`/`pkhOf` calls the  ##
//  ##  Zcash state precompile at 0x...5a00. On a chain without it   ##
//  ##  (anvil, today's Sova testnet) the script stops with          ##
//  ##  "SIP-4 precompile not live". Nothing here is mocked.          ##
//  ###################################################################
//
// ZEC -> SOVA escrow demo on a Sova devnet/testnet (docs/design/zec-escrow.md).
// Signing follows the other scripts: pass --private-key / --account.
//
//   export RPC=http://127.0.0.1:8545  S=script/ZecEscrowDemo.s.sol:ZecEscrowDemo
//
//   0. deploy (once):
//      forge script $S --sig "deploy()" --rpc-url $RPC --broadcast --private-key $MAKER_KEY
//   1. MAKER: new t-address in any Zcash wallet (fresh per order), then lock
//      1 SOVA for 0.5 ZEC, bond 0.01 SOVA, window 40 blocks (~50 min), minConf 3:
//      forge script $S --sig "create(address,string,uint64,uint128,uint64,uint32,uint256)" \
//        $ESCROW tmXXXXXXXX... 50000000 10000000000000000 40 3 1000000000000000000 \
//        --rpc-url $RPC --broadcast --private-key $MAKER_KEY
//   2. TAKER: reserve (posts the bond), THEN pay:
//      forge script $S --sig "reserve(address,uint256)" $ESCROW 1 --rpc-url $RPC --broadcast --private-key $TAKER_KEY
//   3. TAKER: in any Zcash wallet, send >= the price to the t-address that
//      `status` prints (from a shielded balance is fine). Note the txid and
//      the output index (vout) paying that address.
//   4. anyone: watch depth and the deadline:
//      forge script $S --sig "status(address,uint256)" $ESCROW 1 --rpc-url $RPC
//   5. TAKER (or anyone): claim once confirmations >= minConf:
//      forge script $S --sig "claim(address,uint256,bytes32,uint32)" $ESCROW 1 0x<txid-as-explorer-shows> <vout> \
//        --rpc-url $RPC --broadcast --private-key $TAKER_KEY
//   If the window lapses: "expire(address,uint256)" (anyone) or the maker's "cancel(address,uint256)".

import {Script, console} from "forge-std/Script.sol";
import {IZcash, ZCASH_PRECOMPILE} from "../src/zcash/IZcash.sol";
import {ZcashLib} from "../src/zcash/ZcashLib.sol";
import {ZecEscrow} from "../src/zcash/ZecEscrow.sol";

/// @notice Base58Check for transparent P2PKH addresses. Script-side
/// convenience only; the escrow itself takes the 20-byte hash.
/// Prefixes (zebra-chain parameters/network.rs): mainnet t1 = 0x1cb8,
/// testnet/regtest tm = 0x1d25.
library TAddr {
    bytes constant ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    uint16 constant MAINNET_P2PKH = 0x1cb8;
    uint16 constant TESTNET_P2PKH = 0x1d25;

    function encodeP2pkh(bytes20 pkh, bool testnet) internal pure returns (string memory) {
        bytes memory payload = abi.encodePacked(testnet ? TESTNET_P2PKH : MAINNET_P2PKH, pkh);
        bytes memory full = abi.encodePacked(payload, bytes4(sha256(abi.encodePacked(sha256(payload)))));
        uint256 n = uint256(uint208(bytes26(full))); // 26 bytes fit one word; prefix byte is non-zero
        bytes memory out = new bytes(36);
        uint256 i = out.length;
        while (n > 0) {
            out[--i] = ALPHABET[n % 58];
            n /= 58;
        }
        bytes memory s = new bytes(out.length - i);
        for (uint256 k = 0; k < s.length; k++) {
            s[k] = out[i + k];
        }
        return string(s);
    }

    /// @return pkh The 20-byte hash; reverts on bad chars, checksum or prefix.
    function decodeP2pkh(string memory addr) internal pure returns (bytes20 pkh, bool testnet) {
        bytes memory a = bytes(addr);
        uint256 n;
        for (uint256 k = 0; k < a.length; k++) {
            uint256 d = type(uint256).max;
            for (uint256 j = 0; j < 58; j++) {
                if (ALPHABET[j] == a[k]) {
                    d = j;
                    break;
                }
            }
            require(d != type(uint256).max, "TAddr: bad char");
            n = n * 58 + d;
        }
        require(n < (1 << 208), "TAddr: too long");
        bytes26 full = bytes26(uint208(n));
        bytes memory payload = abi.encodePacked(bytes22(full));
        require(bytes4(full << 176) == bytes4(sha256(abi.encodePacked(sha256(payload)))), "TAddr: checksum");
        uint16 prefix = uint16(bytes2(full));
        require(prefix == MAINNET_P2PKH || prefix == TESTNET_P2PKH, "TAddr: not P2PKH");
        pkh = bytes20(full << 16);
        testnet = prefix == TESTNET_P2PKH;
    }
}

contract ZecEscrowDemo is Script {
    function run() external pure {
        revert("use --sig: deploy|create|reserve|status|claim|expire|cancel|tAddr|pkhOf (see header)");
    }

    function _requireSip4() internal view {
        require(ZcashLib.available(), "SIP-4 precompile not live at 0x...5a00: this demo needs it");
    }

    function deploy() external returns (ZecEscrow esc) {
        _requireSip4();
        vm.startBroadcast();
        esc = new ZecEscrow();
        vm.stopBroadcast();
        console.log("ZecEscrow:", address(esc));
    }

    function create(
        ZecEscrow esc,
        string calldata makerTAddr,
        uint64 priceZat,
        uint128 bondWei,
        uint64 windowBlocks,
        uint32 minConf,
        uint256 amountWei
    ) external returns (uint256 id) {
        _requireSip4();
        (bytes20 pkh,) = TAddr.decodeP2pkh(makerTAddr);
        vm.startBroadcast();
        id = esc.createOrder{value: amountWei}(priceZat, pkh, bondWei, windowBlocks, minConf);
        vm.stopBroadcast();
        console.log("order id:", id);
        console.log("pays to: ", makerTAddr);
    }

    function reserve(ZecEscrow esc, uint256 id) external {
        _requireSip4();
        (,,,,,,, uint128 bond,,) = esc.orders(id);
        vm.startBroadcast();
        esc.reserve{value: bond}(id);
        vm.stopBroadcast();
        _status(esc, id);
        console.log("NOW pay the price to the address above. Do not pay after the deadline.");
    }

    function status(ZecEscrow esc, uint256 id) external view {
        _requireSip4();
        _status(esc, id);
    }

    function claim(ZecEscrow esc, uint256 id, bytes32 txid, uint32 vout) external {
        _requireSip4();
        (, uint64 priceZat, uint32 minConf, bytes20 pkh,,,,,,) = esc.orders(id);
        (ZcashLib.Result r, ZcashLib.Payment memory p) =
            ZcashLib.outputPays(txid, vout, ZcashLib.p2pkh(pkh), priceZat, minConf);
        console.log("check result (0 = OK):", uint8(r));
        console.log("confirmations:", p.confirmations, "/", minConf);
        require(r == ZcashLib.Result.OK, "payment not claimable yet (see result code in ZcashLib.Result)");
        vm.startBroadcast();
        esc.claim(id, txid, vout);
        vm.stopBroadcast();
        console.log("claimed order", id);
    }

    function expire(ZecEscrow esc, uint256 id) external {
        _requireSip4();
        vm.startBroadcast();
        esc.expire(id);
        vm.stopBroadcast();
    }

    function cancel(ZecEscrow esc, uint256 id) external {
        _requireSip4();
        vm.startBroadcast();
        esc.cancel(id);
        vm.stopBroadcast();
    }

    /// Works without SIP-4: 20-byte hash -> t-address.
    function tAddr(bytes20 pkh, bool testnet) external pure returns (string memory s) {
        s = TAddr.encodeP2pkh(pkh, testnet);
        console.log(s);
    }

    /// Works without SIP-4: t-address -> 20-byte hash.
    function pkhOf(string calldata addr) external pure returns (bytes20 h) {
        (h,) = TAddr.decodeP2pkh(addr);
        console.logBytes20(h);
    }

    function _status(ZecEscrow esc, uint256 id) internal view {
        (uint64 anchorH,) = IZcash(ZCASH_PRECOMPILE).anchor();
        (
            address maker,
            uint64 priceZat,
            uint32 minConf,
            bytes20 h,
            uint64 window,
            ZecEscrow.Status st,
            uint128 amount,
            uint128 bond,
            address taker,
            uint64 reservedAt
        ) = esc.orders(id);
        console.log("order", id, "status (1 open, 2 filled, 3 cancelled):", uint8(st));
        console.log("maker", maker);
        console.log("SOVA wei / bond wei:", amount, bond);
        console.log("price zat:", priceZat, "minConf:", minConf);
        console.log("pay to (testnet):", TAddr.encodeP2pkh(h, true));
        console.log("pay to (mainnet):", TAddr.encodeP2pkh(h, false));
        console.log("zcash anchor now:", anchorH);
        if (taker != address(0)) {
            console.log("reserved by", taker);
            console.log("reserved at / deadline:", reservedAt, reservedAt + window);
            console.log("payment must be mined above", reservedAt);
        }
    }
}
