// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ZcashStatus} from "./IZcash.sol";
import {ZcashPool} from "./IZcashPools.sol";

/// @dev SIP-7 §4.1 predeploy address.
address constant ZCASH_BLOCKS = 0x0000000000000000000000000000000000005A01;
/// @dev EIP-4788 / EIP-2935 system caller. Only it may call `record`.
address constant SYSTEM_ADDRESS = 0xffffFFFfFFffffffffffffffFfFFFfffFFFfFFfE;

/// @title ZcashBlocks: the anchored Zcash block summaries, in Sova state (SIP-7 §4.1)
/// @notice Canonical system contract at `0x…5A01`. At the start of every
/// Sova block N >= 1 the executor records the summary of Zcash block E_N
/// (the block's SIP-4 anchor) here. The last 8,191 summaries (about 7.1
/// days at 75 s) are kept in a ring and are part of Sova's state root, so
/// `eth_getProof` on this address proves "Zcash block h had these pool
/// totals" to anyone holding a Sova header.
///
/// Anyone can read (`latest`, `window`, `summary`, `summaries`, and the
/// IZcashPools-compatible `poolValue`, `poolTotals`, `blockStats`). Anyone
/// can call `publish` to turn recorded heights into ordinary `ZcashBlock`
/// logs (a system call's own logs are discarded by reth, SIP-7 §4.2).
/// Nobody but SYSTEM_ADDRESS can write. There is no owner, no upgrade, no
/// constructor and no immutable: the runtime bytecode goes straight into
/// the genesis alloc with EMPTY storage (every slot zero), nonce 1 and
/// balance 0.
///
/// ------------------------------------------------------------------
/// THE SYSTEM CALL (node side must match this byte for byte)
/// ------------------------------------------------------------------
/// When: every Sova block N >= 1, in `apply_pre_execution_changes`, after
///   the standard system calls (EIP-4788, EIP-2935) and before the first
///   transaction. Never in block 0 (genesis).
/// How: exactly EIP-4788's `transact_system_call`: caller SYSTEM_ADDRESS
///   (`0xff…fe`), to ZCASH_BLOCKS, value 0, gas limit 30,000,000 not
///   counted against the block, no nonce bump, commit state only. A revert
///   or halt makes the block INVALID (the code is fixed, so only a node bug
///   or inconsistent index data can cause one).
/// Calldata: the Solidity ABI encoding of `record(Block)`, i.e.
///   selector `0x454a0745` =
///   keccak256("record((uint64,bytes32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint64[6],int64[6],uint64[3]))")[:4]
///   followed by 27 static 32-byte words (864 bytes, 868 total), in this
///   order:
///     w0  height          uint64   E_N
///     w1  hash            bytes32  display-order block hash (the anchor hash)
///     w2  time            uint32   header time
///     w3  txCount         uint32   \
///     w4  shieldedTxCount uint32    |
///     w5  tIn             uint32    |
///     w6  tOut            uint32    |  exactly IZcashPools.blockStats(E_N)
///     w7  saplingSpends   uint32    |  fields 1..9, same order
///     w8  saplingOutputs  uint32    |
///     w9  orchardActions  uint32    |
///     w10 ironwoodActions uint32    |
///     w11 joinSplits      uint32   /
///     w12..w17 pools[0..5]  uint64  chainValueZat by ZcashPool id
///                                   (transparent, sprout, sapling, orchard,
///                                   lockbox, ironwood)
///     w18..w23 deltas[0..5] int64   valueDeltaZat by pool id, SIGN-EXTENDED
///                                   to 256 bits (a negative is 0xff…ff…)
///     w24..w26 notes[0..2]  uint64  cumulative tree sizes after E_N
///                                   (sapling, orchard, ironwood)
///   Every word must be clean (uint zero-padded, int64 sign-extended); a
///   dirty word reverts. All values come from the SAME index record the
///   precompile serves for E_N, so `poolTotals(E_N)` / `blockStats(E_N)` /
///   `blockAt(E_N)` on 0x…5A00 and this record agree field for field.
/// Checks (a failure reverts, i.e. the block is invalid):
///   1. msg.sender == SYSTEM_ADDRESS.
///   2. If anything was recorded before: height == previous height + 1
///      (Sova block N anchors E_N and E_{N+1} = E_N + 1; a Zcash reorg
///      unwinds Sova state, this contract's included).
///   3. If anything was recorded before: for every pool,
///      pools[i] == previous pools[i] + deltas[i] (SIP-7 §3 cross-check 1,
///      on chain; catches encoding bugs such as a wrong pool order).
///   4. notes[i] < 2^40 (Zcash note trees have depth 32, so at most 2^32).
///   The first record ever is accepted at any height (it is E_1 = B).
/// Returns nothing.
///
/// ------------------------------------------------------------------
/// STORAGE LAYOUT (for eth_getProof / eth_getStorageAt readers)
/// ------------------------------------------------------------------
/// slot 0 (head): bits 0..63 newest recorded height; bits 64..127 first
///   height ever recorded; bit 128 = 1 once anything is recorded. Zero at
///   genesis.
/// Ring entry for height h: six slots starting at
///   base(h) = 1 + 6 * (h mod 8191); slots 1 .. 49,146 in total.
///   A slot's "bits a..b" count from the least significant bit.
///   base+0: hash (bytes32, display order)
///   base+1: height u64 [0..63] | time u32 [64..95] | txCount u32 [96..127]
///           | shieldedTxCount u32 [128..159] | tIn u32 [160..191]
///           | tOut u32 [192..223] | joinSplits u32 [224..255]
///   base+2: pools[0] transparent [0..63] | pools[1] sprout [64..127]
///           | pools[2] sapling [128..191] | pools[3] orchard [192..255]
///   base+3: pools[4] lockbox [0..63] | pools[5] ironwood [64..127]
///           | saplingSpends u32 [128..159] | saplingOutputs u32 [160..191]
///           | orchardActions u32 [192..223] | ironwoodActions u32 [224..255]
///   base+4: deltas[0..3] as 64-bit two's complement, 64 bits each
///           (transparent [0..63], sprout, sapling, orchard [192..255])
///   base+5: deltas[4] lockbox [0..63] | deltas[5] ironwood [64..127]
///           | saplingNotes u40 [128..167] | orchardNotes u40 [168..207]
///           | ironwoodNotes u40 [208..247] | published flag [248]
///           | bits 249..255 zero
///   An entry is valid for h iff its height field equals h and h is inside
///   window() (oldest = max(first, newest - 8190)).
///
/// Cost of the system call (forge --isolate, test/ZcashBlocks.t.sol,
/// execution only): about 154k gas-equivalent per block while the ring
/// fills (six zero->nonzero SSTOREs), about 48k once it has wrapped
/// (six nonzero->nonzero). No user-visible gas.
contract ZcashBlocks {
    /// @notice Ring size in Zcash blocks (about 7.1 days at 75 s).
    uint64 public constant RING = 8191;
    /// @notice Most summaries `summaries` returns in one call.
    uint64 public constant MAX_RANGE = 1024;

    /// @notice One anchored Zcash block. Field order is the `record` ABI.
    struct Block {
        uint64 height;
        bytes32 hash;
        uint32 time;
        uint32 txCount;
        uint32 shieldedTxCount;
        uint32 tIn;
        uint32 tOut;
        uint32 saplingSpends;
        uint32 saplingOutputs;
        uint32 orchardActions;
        uint32 ironwoodActions;
        uint32 joinSplits;
        uint64[6] pools;
        int64[6] deltas;
        uint64[3] notes;
    }

    /// @notice IZcashPools.blockStats return tuple, as a struct.
    struct Stats {
        uint8 status;
        uint32 txCount;
        uint32 shieldedTxCount;
        uint32 tIn;
        uint32 tOut;
        uint32 saplingSpends;
        uint32 saplingOutputs;
        uint32 orchardActions;
        uint32 ironwoodActions;
        uint32 joinSplits;
        uint64 saplingNotes;
        uint64 orchardNotes;
        uint64 ironwoodNotes;
    }

    /// @notice A recorded Zcash block, emitted once by `publish`. The data
    /// is the full summary (27 words). Topic 0 is
    /// keccak256("ZcashBlock(uint64,(uint64,bytes32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint32,uint64[6],int64[6],uint64[3]))")
    /// = 0xa13343acdebcc5ad91f6b66b8851f7f8870975616bf4339a7c829bfa93952b53.
    event ZcashBlock(uint64 indexed height, Block summary);

    error NotSystem();
    error NotNext(uint64 expected, uint64 got);
    error DeltaMismatch(uint8 pool);
    error NotesTooLarge();

    uint256 private constant FLAG_RECORDED = 1 << 128;
    uint256 private constant FLAG_PUBLISHED = 1 << 248;
    uint256 private constant M64 = type(uint64).max;
    uint256 private constant M40 = (1 << 40) - 1;
    uint256 private constant M32 = type(uint32).max;

    // slot 0
    uint256 private _head;
    // slots 1 .. 49,146: _ring[i][j] is slot 1 + 6 * i + j
    uint256[6][8191] private _ring;

    // ------------------------------------------------------------------
    // Write (system caller only)
    // ------------------------------------------------------------------

    /// @notice Record the anchored Zcash block of the current Sova block.
    /// Called once per Sova block by the executor; see the contract NatSpec
    /// for the exact calldata and checks.
    function record(Block calldata b) external {
        if (msg.sender != SYSTEM_ADDRESS) revert NotSystem();
        uint64 h = b.height;
        uint256 head = _head;
        if (head & FLAG_RECORDED != 0) {
            uint64 newest = uint64(head);
            if (h != newest + 1) revert NotNext(newest + 1, h);
            uint256[6] storage prev = _ring[newest % RING];
            uint256 p2 = prev[2];
            uint256 p3 = prev[3];
            for (uint256 i = 0; i < 6; i++) {
                uint256 pv = i < 4 ? (p2 >> (64 * i)) & M64 : (p3 >> (64 * (i - 4))) & M64;
                if (int256(pv) + int256(b.deltas[i]) != int256(uint256(b.pools[i]))) revert DeltaMismatch(uint8(i));
            }
            _head = (head & ~M64) | h;
        } else {
            _head = FLAG_RECORDED | (uint256(h) << 64) | h;
        }
        if (b.notes[0] > M40 || b.notes[1] > M40 || b.notes[2] > M40) revert NotesTooLarge();

        uint256[6] storage e = _ring[h % RING];
        e[0] = uint256(b.hash);
        e[1] = uint256(h) | (uint256(b.time) << 64) | (uint256(b.txCount) << 96) | (uint256(b.shieldedTxCount) << 128)
            | (uint256(b.tIn) << 160) | (uint256(b.tOut) << 192) | (uint256(b.joinSplits) << 224);
        e[2] = uint256(b.pools[0]) | (uint256(b.pools[1]) << 64) | (uint256(b.pools[2]) << 128)
            | (uint256(b.pools[3]) << 192);
        e[3] = uint256(b.pools[4]) | (uint256(b.pools[5]) << 64) | (uint256(b.saplingSpends) << 128)
            | (uint256(b.saplingOutputs) << 160) | (uint256(b.orchardActions) << 192) | (uint256(b.ironwoodActions) << 224);
        e[4] = _i64(b.deltas[0]) | (_i64(b.deltas[1]) << 64) | (_i64(b.deltas[2]) << 128) | (_i64(b.deltas[3]) << 192);
        e[5] = _i64(b.deltas[4]) | (_i64(b.deltas[5]) << 64) | (uint256(b.notes[0]) << 128)
            | (uint256(b.notes[1]) << 168) | (uint256(b.notes[2]) << 208);
    }

    // ------------------------------------------------------------------
    // Reads
    // ------------------------------------------------------------------

    /// @notice Newest recorded Zcash block: E_N of the current Sova block
    /// (the pre-block call runs before any transaction). (0, 0) before the
    /// first record.
    function latest() external view returns (uint64 height, bytes32 hash) {
        uint256 head = _head;
        if (head & FLAG_RECORDED == 0) return (0, bytes32(0));
        height = uint64(head);
        hash = bytes32(_ring[height % RING][0]);
    }

    /// @notice Heights readable here: [oldest, newest]. (0, 0) when empty.
    function window() public view returns (uint64 oldest, uint64 newest) {
        uint256 head = _head;
        if (head & FLAG_RECORDED == 0) return (0, 0);
        newest = uint64(head);
        uint64 first = uint64(head >> 64);
        oldest = newest - first >= RING - 1 ? newest - (RING - 1) : first;
    }

    /// @notice The summary of Zcash block `h`; ok = false (and a zeroed
    /// summary) outside window().
    function summary(uint64 h) public view returns (bool ok, Block memory b) {
        (uint8 st,) = _status(h);
        if (st != ZcashStatus.OK) return (false, b);
        return (true, _load(h));
    }

    /// @notice Summaries for [fromH, toH] clipped to window(), oldest
    /// first, at most MAX_RANGE of them (the NEWEST MAX_RANGE when the
    /// clipped range is longer). Empty when nothing overlaps.
    function summaries(uint64 fromH, uint64 toH) external view returns (Block[] memory out) {
        (uint64 lo, uint64 hi) = window();
        if (_head & FLAG_RECORDED == 0 || fromH > toH || toH < lo || fromH > hi) return out;
        if (fromH < lo) fromH = lo;
        if (toH > hi) toH = hi;
        if (toH - fromH + 1 > MAX_RANGE) fromH = toH - MAX_RANGE + 1;
        out = new Block[](toH - fromH + 1);
        for (uint256 i = 0; i < out.length; i++) {
            out[i] = _load(fromH + uint64(i));
        }
    }

    /// @notice IZcashPools.poolValue over the ring. Status: NOT_YET if
    /// h > newest (or nothing recorded), OUT_OF_RANGE if h < oldest (the
    /// precompile still answers those), then NO_SUCH_POOL if pool > 5.
    function poolValue(uint64 h, uint8 pool)
        external
        view
        returns (uint8 status, uint64 chainValueZat, int64 deltaZat)
    {
        uint256 slot;
        (status, slot) = _status(h);
        if (status != ZcashStatus.OK) return (status, 0, 0);
        if (pool >= ZcashPool.COUNT) return (ZcashStatus.NO_SUCH_POOL, 0, 0);
        uint256[6] storage e = _ring[slot];
        uint256 p = pool;
        chainValueZat = uint64(p < 4 ? e[2] >> (64 * p) : e[3] >> (64 * (p - 4)));
        deltaZat = int64(uint64(p < 4 ? e[4] >> (64 * p) : e[5] >> (64 * (p - 4))));
    }

    /// @notice IZcashPools.poolTotals over the ring (arrays of length 6).
    function poolTotals(uint64 h)
        external
        view
        returns (uint8 status, uint64[] memory chainValueZat, int64[] memory deltaZat)
    {
        uint256 slot;
        (status, slot) = _status(h);
        if (status != ZcashStatus.OK) return (status, chainValueZat, deltaZat);
        uint256[6] storage e = _ring[slot];
        (uint256 s2, uint256 s3, uint256 s4, uint256 s5) = (e[2], e[3], e[4], e[5]);
        chainValueZat = new uint64[](6);
        deltaZat = new int64[](6);
        for (uint256 i = 0; i < 4; i++) {
            chainValueZat[i] = uint64(s2 >> (64 * i));
            deltaZat[i] = int64(uint64(s4 >> (64 * i)));
        }
        chainValueZat[4] = uint64(s3);
        chainValueZat[5] = uint64(s3 >> 64);
        deltaZat[4] = int64(uint64(s5));
        deltaZat[5] = int64(uint64(s5 >> 64));
    }

    /// @notice IZcashPools.blockStats over the ring. Returned as a static
    /// struct, whose ABI encoding is byte-identical to the interface's flat
    /// 13-value tuple (a static tuple encodes inline), so callers of
    /// IZcashPools.blockStats decode it unchanged.
    function blockStats(uint64 h) external view returns (Stats memory s) {
        uint256 slot;
        (s.status, slot) = _status(h);
        if (s.status != ZcashStatus.OK) return s;
        uint256[6] storage e = _ring[slot];
        (uint256 s1, uint256 s3, uint256 s5) = (e[1], e[3], e[5]);
        s.txCount = uint32(s1 >> 96);
        s.shieldedTxCount = uint32(s1 >> 128);
        s.tIn = uint32(s1 >> 160);
        s.tOut = uint32(s1 >> 192);
        s.saplingSpends = uint32(s3 >> 128);
        s.saplingOutputs = uint32(s3 >> 160);
        s.orchardActions = uint32(s3 >> 192);
        s.ironwoodActions = uint32(s3 >> 224);
        s.joinSplits = uint32(s1 >> 224);
        s.saplingNotes = uint64((s5 >> 128) & M40);
        s.orchardNotes = uint64((s5 >> 168) & M40);
        s.ironwoodNotes = uint64((s5 >> 208) & M40);
    }

    /// @notice True iff `h` is in window() and has been published.
    function isPublished(uint64 h) external view returns (bool) {
        (uint8 st, uint256 slot) = _status(h);
        return st == ZcashStatus.OK && _ring[slot][5] & FLAG_PUBLISHED != 0;
    }

    // ------------------------------------------------------------------
    // Feed (anyone)
    // ------------------------------------------------------------------

    /// @notice Emit one `ZcashBlock` log for every height in [fromH, toH]
    /// that is inside window() and not yet published, then mark it. Anyone
    /// can call (a sealer, a keeper, a dapp); idempotent per height, so
    /// overlapping calls never emit a height twice and gaps can be filled
    /// later. The logs land in the Sova block that ran `publish`, so
    /// eth_getLogs / eth_subscribe("logs") tooling sees them. A Sova reorg
    /// rolls the published flags back with everything else.
    /// @return emitted Number of logs emitted.
    function publish(uint64 fromH, uint64 toH) external returns (uint256 emitted) {
        (uint64 lo, uint64 hi) = window();
        if (_head & FLAG_RECORDED == 0 || fromH > toH || toH < lo || fromH > hi) return 0;
        if (fromH < lo) fromH = lo;
        if (toH > hi) toH = hi;
        for (uint256 i = 0; i <= toH - fromH; i++) {
            uint64 h = fromH + uint64(i);
            uint256[6] storage e = _ring[h % RING];
            uint256 s5 = e[5];
            if (s5 & FLAG_PUBLISHED != 0) continue;
            e[5] = s5 | FLAG_PUBLISHED;
            emit ZcashBlock(h, _load(h));
            emitted++;
        }
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    /// @dev Window status for h, and its ring index when OK.
    function _status(uint64 h) private view returns (uint8, uint256) {
        (uint64 lo, uint64 hi) = window();
        if (_head & FLAG_RECORDED == 0 || h > hi) return (ZcashStatus.NOT_YET, 0);
        if (h < lo) return (ZcashStatus.OUT_OF_RANGE, 0);
        return (ZcashStatus.OK, h % RING);
    }

    function _load(uint64 h) private view returns (Block memory b) {
        uint256[6] storage e = _ring[h % RING];
        (uint256 s1, uint256 s2, uint256 s3, uint256 s4, uint256 s5) = (e[1], e[2], e[3], e[4], e[5]);
        b.hash = bytes32(e[0]);
        b.height = uint64(s1);
        b.time = uint32(s1 >> 64);
        b.txCount = uint32(s1 >> 96);
        b.shieldedTxCount = uint32(s1 >> 128);
        b.tIn = uint32(s1 >> 160);
        b.tOut = uint32(s1 >> 192);
        b.joinSplits = uint32(s1 >> 224);
        b.saplingSpends = uint32(s3 >> 128);
        b.saplingOutputs = uint32(s3 >> 160);
        b.orchardActions = uint32(s3 >> 192);
        b.ironwoodActions = uint32(s3 >> 224);
        for (uint256 i = 0; i < 4; i++) {
            b.pools[i] = uint64(s2 >> (64 * i));
            b.deltas[i] = int64(uint64(s4 >> (64 * i)));
        }
        b.pools[4] = uint64(s3);
        b.pools[5] = uint64(s3 >> 64);
        b.deltas[4] = int64(uint64(s5));
        b.deltas[5] = int64(uint64(s5 >> 64));
        b.notes[0] = uint64((s5 >> 128) & M40);
        b.notes[1] = uint64((s5 >> 168) & M40);
        b.notes[2] = uint64((s5 >> 208) & M40);
    }

    function _i64(int64 v) private pure returns (uint256) {
        return uint256(uint64(v));
    }
}
