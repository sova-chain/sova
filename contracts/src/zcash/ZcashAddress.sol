// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title ZcashAddress: decode a transparent Zcash address on-chain
/// @notice Base58Check t-addresses: 2-byte prefix, 20-byte hash, 4-byte
/// double-SHA256 checksum. Prefixes (zebra-chain parameters/network.rs):
///   mainnet  t1 P2PKH 0x1cb8   t3 P2SH 0x1cbd
///   testnet  tm P2PKH 0x1d25   t2 P2SH 0x1cba   (regtest uses testnet's)
/// Meant for constructors (a few hundred thousand gas): contracts that
/// take a human-readable payee decode it once and keep the hash.
library ZcashAddress {
    bytes private constant ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    error BadZcashAddress();

    /// @return hash The 20-byte pubkey hash (P2PKH) or script hash (P2SH).
    /// @return p2sh True for t3 / t2.
    /// @return mainnet True for t1 / t3.
    function decode(string memory addr) internal pure returns (bytes20 hash, bool p2sh, bool mainnet) {
        bytes memory a = bytes(addr);
        if (a.length < 34 || a.length > 36) revert BadZcashAddress();
        uint256 n;
        for (uint256 k = 0; k < a.length; k++) {
            uint256 d = 58;
            for (uint256 j = 0; j < 58; j++) {
                if (ALPHABET[j] == a[k]) {
                    d = j;
                    break;
                }
            }
            if (d == 58) revert BadZcashAddress();
            n = n * 58 + d;
            if (n >= 1 << 208) revert BadZcashAddress();
        }
        bytes26 full = bytes26(uint208(n));
        bytes memory payload = abi.encodePacked(bytes22(full));
        if (bytes4(full << 176) != bytes4(sha256(abi.encodePacked(sha256(payload))))) revert BadZcashAddress();
        uint16 prefix = uint16(bytes2(full));
        if (prefix == 0x1cb8) (p2sh, mainnet) = (false, true);
        else if (prefix == 0x1cbd) (p2sh, mainnet) = (true, true);
        else if (prefix == 0x1d25) (p2sh, mainnet) = (false, false);
        else if (prefix == 0x1cba) (p2sh, mainnet) = (true, false);
        else revert BadZcashAddress();
        hash = bytes20(full << 16);
    }
}
