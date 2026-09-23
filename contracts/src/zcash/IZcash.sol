// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @dev Provisional SIP-4 precompile address (SIP-4 §3).
address constant ZCASH_PRECOMPILE = 0x0000000000000000000000000000000000005a00;

/// @title SIP-4 status codes
/// @notice Every IZcash lookup returns one of these as its first value.
/// A non-OK status is a *result*, identical on every honest node; it is
/// never an RPC failure (a node that cannot answer holds the block).
/// On a non-OK status every other returned field is zero / empty.
library ZcashStatus {
    /// @dev The answer is in the anchored segment Z[B .. E_N].
    uint8 internal constant OK = 0;
    /// @dev Not in Z[B .. E_N] on the anchored chain. For txid-keyed
    /// methods this includes "mined above E_N" and "mined before B":
    /// a node never reveals index entries past the anchor.
    uint8 internal constant NOT_FOUND = 1;
    /// @dev Height-keyed: h > E_N (the anchor has not reached it yet).
    uint8 internal constant NOT_YET = 2;
    /// @dev Height-keyed: h < B (v1 indexes only from the epoch base).
    uint8 internal constant OUT_OF_RANGE = 3;
    /// @dev txOutput: the tx is found but has no transparent output `vout`
    /// (vout >= nOut; a fully shielded tx has nOut = 0).
    uint8 internal constant NO_SUCH_OUTPUT = 4;
    /// @dev burnInfo: the tx is found but is not a SIP-1 burn.
    uint8 internal constant NOT_A_BURN = 5;
}

/// @title IZcash: SIP-4 Zcash state precompile (v1)
/// @notice Read-only view of *transparent* Zcash chain state, as a pure
/// function of the Zcash chain prefix the executing Sova block commits to.
/// Sova block N anchors Zcash block E_N = N + B - 1, where B is the epoch
/// base; answers cover Z[B .. E_N] and nothing else (no tip, no mempool,
/// no "unspent now").
///
/// Conventions (the node side must match these byte for byte):
/// - ABI: ordinary Solidity ABI, 4-byte selectors, standard return tuple
///   encoding, every word clean (zero-padded). Negative answers return the
///   full tuple with zeroed fields (for txOutput: an empty `bytes`), never
///   empty returndata. Malformed calldata reverts. `value > 0` reverts.
///   Callers use STATICCALL (all methods are `view`).
/// - txid and block hashes are DISPLAY-ORDER bytes: the hex string an
///   explorer or zebrad RPC prints, read left to right, is the bytes32 from
///   its most significant byte down. So txid "ab12..ef" is
///   `bytes32(0xab12...ef)`. This is the reverse of Zcash's internal
///   (wire) byte order.
/// - Values are integer zatoshis (zebrad `valueZat`; 1 ZEC = 1e8 zat).
/// - Scripts are raw scriptPubKey bytes (not hex text, not ASM).
/// - Confirmations are E_N - height + 1, computed from the anchor, never
///   zebrad's tip-relative `confirmations` field.
///
/// Library caveats (see ZcashLib): pre-v5 txids are malleable before they
/// are mined, and coinbase transactions are included. Recommended
/// confirmation depths: testnet 3, mainnet 10 (about 12.5 min at 75 s),
/// more for large values.
interface IZcash {
    /// @notice The Zcash block this Sova block commits to.
    /// @return height E_N.
    /// @return hash Display-order hash of Zcash block E_N (equals
    /// `header.parent_beacon_block_root`).
    function anchor() external view returns (uint64 height, bytes32 hash);

    /// @notice Header data at a Zcash height.
    /// @return status OK for B <= h <= E_N; NOT_YET for h > E_N;
    /// OUT_OF_RANGE for h < B.
    /// @return hash Display-order block hash.
    /// @return time Header `time` (miner-set; not monotonic).
    function blockAt(uint64 h) external view returns (uint8 status, bytes32 hash, uint32 time);

    /// @notice Where a transaction was mined. Works for any tx, fully
    /// shielded ones included (txid, height and position are public).
    /// @return status OK or NOT_FOUND.
    /// @return height Zcash height of the containing block.
    /// @return index Position of the tx in its block (coinbase = 0).
    /// @return confirmations E_N - height + 1 (>= 1 when OK).
    /// @return nOut Number of transparent outputs (vout count).
    /// @return version Transaction version (4, 5, 6, ...).
    function txInfo(bytes32 txid)
        external
        view
        returns (uint8 status, uint64 height, uint32 index, uint64 confirmations, uint32 nOut, uint32 version);

    /// @notice A transparent output of a mined transaction.
    /// @return status OK, NOT_FOUND (tx unknown) or NO_SUCH_OUTPUT
    /// (vout >= nOut).
    /// @return valueZat Output value in zatoshis.
    /// @return script Raw scriptPubKey bytes (<= 10,000 bytes).
    function txOutput(bytes32 txid, uint32 vout)
        external
        view
        returns (uint8 status, uint64 valueZat, bytes memory script);

    /// @notice The SIP-1 burn carried by a tx, via the exact consensus
    /// parser (`sip1::extract_burn`).
    /// @return status OK, NOT_FOUND (tx unknown or outside Z[B .. E_N]),
    /// or NOT_A_BURN (tx found, not a SIP-1 burn).
    /// @return credited Sova address the burn credits.
    /// @return signal SIP-1 signal field.
    /// @return weightZat Burned weight in zatoshis.
    function burnInfo(bytes32 txid)
        external
        view
        returns (uint8 status, address credited, uint32 signal, uint64 weightZat);

    // Reserved for v1.1 (not in v1; do not call):
    // function spentBy(bytes32 txid, uint32 vout)
    //     external view returns (uint8 status, bytes32 spender, uint64 height);
    // Outputs created at >= B; spentness as of E_N only.
}
