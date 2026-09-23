// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

//  ###################################################################
//  ##  NEEDS SIP-4 LIVE (anchor + txInfo + txOutput at 0x...5a00).   ##
//  ##  On a chain without it the script stops with "SIP-4 ... not   ##
//  ##  live". Nothing here is mocked.                                ##
//  ###################################################################
//
// Buy an Ashwing with ZEC (docs/design/ashwing-zec-checkout.md).
// Signing follows the other scripts: pass --private-key / --account.
//
//   export RPC=http://127.0.0.1:8545  S=script/AshwingZecCheckoutDemo.s.sol:AshwingZecCheckoutDemo
//
//   0. deploy (once), pointing at the Ashwings collection:
//      forge script $S --sig "deploy(address)" $ASHWINGS --rpc-url $RPC --broadcast --private-key $KEY
//   1. SELLER: list at 0.25 ZEC to your own t-address, window 40 (~50 min), minConf 3:
//      forge script $S --sig "list(address,string,uint64,uint32,uint16)" $CO tmXXXX... 25000000 40 3 \
//        --rpc-url $RPC --broadcast --private-key $SELLER_KEY
//   2. BUYER (or a relayer for them): reserve; prints the exact amount and a ZIP-321 URI:
//      forge script $S --sig "reserve(address,uint256,address)" $CO 1 $BUYER --rpc-url $RPC --broadcast --private-key $KEY
//   3. BUYER: pay that exact amount to that address from any Zcash wallet (shielded balance is fine).
//   4. anyone: status (depth, deadline):
//      forge script $S --sig "status(address,uint256)" $CO <reservationId> --rpc-url $RPC
//   5. anyone: claim once deep enough; the owl goes to the buyer:
//      forge script $S --sig "claim(address,uint256,bytes32,uint32)" $CO <reservationId> 0x<txid> <vout> \
//        --rpc-url $RPC --broadcast --private-key $KEY

import {Script, console} from "forge-std/Script.sol";
import {IZcash, ZCASH_PRECOMPILE} from "../src/zcash/IZcash.sol";
import {ZcashLib} from "../src/zcash/ZcashLib.sol";
import {AshwingsZecCheckout} from "../src/zcash/ZecCheckout.sol";
import {TAddr} from "./ZecEscrowDemo.s.sol";

library ZecFmt {
    /// @notice Zatoshis as a ZEC decimal with all 8 places ("0.25000001").
    function zec(uint64 zat) internal pure returns (string memory) {
        bytes memory frac = new bytes(8);
        uint64 f = zat % 1e8;
        for (uint256 i = 8; i > 0; i--) {
            frac[i - 1] = bytes1(uint8(48 + f % 10));
            f /= 10;
        }
        return string.concat(_u(zat / 1e8), ".", string(frac));
    }

    /// @notice ZIP-321 payment URI: wallets scan it and fill address and amount.
    function uri(string memory tAddr, uint64 zat) internal pure returns (string memory) {
        return string.concat("zcash:", tAddr, "?amount=", zec(zat));
    }

    function _u(uint256 v) private pure returns (string memory) {
        if (v == 0) return "0";
        bytes memory b = new bytes(20);
        uint256 i = 20;
        while (v > 0) {
            b[--i] = bytes1(uint8(48 + v % 10));
            v /= 10;
        }
        bytes memory s = new bytes(20 - i);
        for (uint256 k = 0; k < s.length; k++) {
            s[k] = b[i + k];
        }
        return string(s);
    }
}

contract AshwingZecCheckoutDemo is Script {
    function run() external pure {
        revert("use --sig: deploy|list|reserve|status|claim (see header)");
    }

    function _requireSip4() internal view {
        require(ZcashLib.available(), "SIP-4 precompile not live at 0x...5a00: this demo needs it");
    }

    function deploy(address ashwings) external returns (AshwingsZecCheckout co) {
        vm.startBroadcast();
        co = new AshwingsZecCheckout(ashwings);
        vm.stopBroadcast();
        console.log("AshwingsZecCheckout:", address(co));
    }

    /// P2PKH t-address only (t1... / tm...), the kind every wallet shows.
    function list(AshwingsZecCheckout co, string calldata sellerTAddr, uint64 priceZat, uint32 window, uint16 minConf)
        external
        returns (uint256 id)
    {
        (bytes20 pkh,) = TAddr.decodeP2pkh(sellerTAddr);
        vm.startBroadcast();
        id = co.list(priceZat, pkh, false, window, minConf);
        vm.stopBroadcast();
        console.log("listing id:", id);
    }

    function reserve(AshwingsZecCheckout co, uint256 listingId, address recipient) external returns (uint256 id) {
        _requireSip4();
        vm.startBroadcast();
        id = co.reserve(listingId, recipient);
        vm.stopBroadcast();
        _status(co, id);
        console.log("NOW pay EXACTLY that amount. The payment must be mined by the deadline.");
    }

    function status(AshwingsZecCheckout co, uint256 id) external view {
        _requireSip4();
        _status(co, id);
    }

    function claim(AshwingsZecCheckout co, uint256 id, bytes32 txid, uint32 vout) external {
        _requireSip4();
        (,, uint16 minConf,,,,,) = co.reservations(id);
        (ZcashLib.Result r, ZcashLib.Payment memory p) =
            ZcashLib.outputPays(txid, vout, co.payeeScript(id), co.quote(id), minConf);
        console.log("check result (0 = OK):", uint8(r));
        console.log("confirmations:", p.confirmations, "/", minConf);
        require(r == ZcashLib.Result.OK, "payment not claimable yet (see ZcashLib.Result)");
        vm.startBroadcast();
        uint256 owl = co.claim(id, txid, vout);
        vm.stopBroadcast();
        console.log("Ashwing minted:", owl);
    }

    function _status(AshwingsZecCheckout co, uint256 id) internal view {
        (uint64 anchorH,) = IZcash(ZCASH_PRECOMPILE).anchor();
        (address recipient, uint64 quoteZat, uint16 minConf, bool p2sh, bool filled, bytes20 h, uint64 at,) =
            co.reservations(id);
        require(!p2sh, "P2SH payee: encode the t3/t2 address yourself");
        console.log("reservation", id, filled ? "FILLED" : "open");
        console.log("owl goes to", recipient);
        console.log("pay exactly (ZEC):", ZecFmt.zec(quoteZat));
        console.log("testnet URI:", ZecFmt.uri(TAddr.encodeP2pkh(h, true), quoteZat));
        console.log("mainnet URI:", ZecFmt.uri(TAddr.encodeP2pkh(h, false), quoteZat));
        console.log("mined in (reservedAt, deadline]:", at, co.deadline(id));
        console.log("minConf / zcash anchor now:", minConf, anchorH);
    }
}
