// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ZcashLib} from "./ZcashLib.sol";

/// @title ZecCheckout: pay ZEC on Zcash, get something on Sova (SIP-4 use case 2)
/// @notice A seller lists an item at a ZEC price and names one transparent
/// Zcash address of their own. A buyer reserves (free), pays an exact,
/// per-reservation amount to that address from any Zcash wallet (including
/// from a shielded balance: z->t), and anyone claims by pointing at the
/// payment. The contract reads the payment through the SIP-4 precompile and
/// delivers. No bridge, no wrapped token: the ZEC goes from buyer to seller
/// on Zcash and never leaves Zcash.
///
/// One address, many buyers: amount tags. Prices are whole multiples of
/// TAG_SPACE (100,000 zat = 0.001 ZEC). Each reservation gets a tag in
/// [1, TAG_SPACE) from a counter keyed by (payee script, price), and its
/// quote is price + tag, paid EXACTLY. Price and tag decode uniquely from
/// the quote, and tags are never reused, so every (payee script, amount)
/// pair belongs to at most one reservation, ever, in this contract. Hence:
/// - many buyers share one seller address concurrently;
/// - a reservation locks nothing scarce (no address, no inventory for an
///   open mint), so there is nothing to grief and no bond;
/// - a late payment can never be claimed by a later reserver (the
///   ZecEscrow reuse hazard is gone): its amount matches only its own
///   reservation.
/// The global (txid, vout) set is kept anyway as a second, independent
/// one-payment-one-fill guard.
///
/// Clock. Time is the Zcash anchor height (ZcashLib.anchorHeight). A
/// reservation made at anchor A accepts a payment MINED at a height h with
/// A < h <= A + window. The window bounds how long a quoted price holds;
/// confirmations may accrue after it, so claim has no deadline.
///
/// Rules. No owner, no admin, no fees, no upgrade. A listing is controlled
/// by its seller only, and every reservation snapshots the listing's terms
/// (price, payee, window, minConf), so later edits never touch it.
///
/// Honest limits:
/// - Exact amount. Overpaying or underpaying by one zatoshi is not
///   claimable (the ZEC still reached the seller; any refund is off-chain).
///   Use a ZIP-321 payment URI / QR so the wallet fills the amount. Exchange
///   withdrawals that net their fee out of the amount will not work.
/// - Pay inside the window. A payment mined after A + window is not
///   claimable. A Zcash reorg that re-mines a payment later can push it
///   past the window, so leave slack.
/// - Transparent payee. The seller's address and every sale amount are
///   public on Zcash (they are public on Sova anyway). Sweep to shielded.
/// - Reorgs deeper than minConf can undo a claim on Sova (SIP-4 reorgs Sova
///   with Zcash) but not anything done off Sova meanwhile.
/// - Uniqueness is per contract. Using the same payee address with
///   another contract at the same time is the seller's risk.
/// - Tag exhaustion. TAG_SPACE - 1 reservations per (payee, price). Free
///   reservations let a spammer burn them for gas; the seller then moves
///   the price by 0.001 ZEC or rotates the address.
abstract contract ZecCheckout {
    /// @notice Price granularity and tag range, in zatoshis (0.001 ZEC).
    uint64 public constant TAG_SPACE = 100_000;

    struct Listing {
        // slot 0
        address seller;
        uint64 priceZat; // multiple of TAG_SPACE
        uint16 minConf;
        bool payeeP2sh;
        bool active;
        // slot 1
        bytes20 payeeHash;
        uint32 window;
    }

    struct Reservation {
        // slot 0
        address recipient; // 0 = no such reservation
        uint64 quoteZat; // exact amount to pay: price + tag
        uint16 minConf;
        bool payeeP2sh;
        bool filled;
        // slot 1
        bytes20 payeeHash;
        uint64 reservedAt;
        uint32 window;
    }

    /// @notice Listings by id; ids start at 1.
    mapping(uint256 => Listing) public listings;
    uint256 public listingCount;

    /// @notice Reservations by id; ids start at 1.
    mapping(uint256 => Reservation) public reservations;
    uint256 public reservationCount;

    /// @notice keccak256(abi.encode(payeeP2sh, payeeHash, priceZat)) => last tag issued.
    mapping(bytes32 => uint64) public lastTag;

    /// @notice keccak256(abi.encode(txid, vout)) => reservation it filled (0 = unused).
    mapping(bytes32 => uint256) public paymentUsed;

    event Listed(uint256 indexed listingId, address indexed seller);
    event ListingUpdated(
        uint256 indexed listingId,
        uint64 priceZat,
        bytes20 payeeHash,
        bool payeeP2sh,
        uint32 window,
        uint16 minConf,
        bool active
    );
    event Reserved(
        uint256 indexed reservationId,
        uint256 indexed listingId,
        address indexed recipient,
        uint64 quoteZat,
        bytes20 payeeHash,
        bool payeeP2sh,
        uint64 reservedAt,
        uint64 deadline
    );
    event Claimed(
        uint256 indexed reservationId,
        address indexed recipient,
        bytes32 txid,
        uint32 vout,
        uint64 height,
        uint256 itemId
    );

    error BadParams();
    error NotSeller();
    error ListingInactive();
    error ZeroRecipient();
    error TagsExhausted();
    error NoSuchReservation();
    error AlreadyFilled();
    error PaymentAlreadyUsed(uint256 filledReservation);
    error WrongAmount(uint64 valueZat, uint64 quoteZat);
    error PaidBeforeReservation(uint64 paymentHeight, uint64 reservedAt);
    error PaidAfterDeadline(uint64 paymentHeight, uint64 deadline);

    // ------------------------------------------------------------------
    // Seller
    // ------------------------------------------------------------------

    /// @notice Open a listing paid to the seller's own t-address.
    /// @param priceZat Price in zatoshis, a positive multiple of TAG_SPACE.
    /// @param payeeHash 20-byte hash of the t-address (P2PKH pubkey hash or
    /// P2SH script hash).
    /// @param payeeP2sh True for P2SH (t3.../t2...), false for P2PKH (t1.../tm...).
    /// @param window Zcash blocks (~75 s each) a reservation's price holds.
    /// @param minConf Payment depth (>= 1). Testnet 3, mainnet 10.
    function list(uint64 priceZat, bytes20 payeeHash, bool payeeP2sh, uint32 window, uint16 minConf)
        external
        returns (uint256 id)
    {
        id = ++listingCount;
        listings[id].seller = msg.sender;
        emit Listed(id, msg.sender);
        _set(id, priceZat, payeeHash, payeeP2sh, window, minConf, true);
    }

    /// @notice Change any term of your listing, or pause it (active = false).
    /// Existing reservations keep the terms they were made under.
    function update(
        uint256 id,
        uint64 priceZat,
        bytes20 payeeHash,
        bool payeeP2sh,
        uint32 window,
        uint16 minConf,
        bool active
    ) external {
        if (listings[id].seller != msg.sender) revert NotSeller();
        _set(id, priceZat, payeeHash, payeeP2sh, window, minConf, active);
    }

    // ------------------------------------------------------------------
    // Buyer
    // ------------------------------------------------------------------

    /// @notice Reserve one item for `recipient`. Free, and anyone may call
    /// (a relayer can reserve for a buyer who holds no SOVA). Returns the
    /// reservation id; read the exact amount and payee from {Reserved} or
    /// {quote} / {payeeScript}. Pay only AFTER this is mined.
    function reserve(uint256 listingId, address recipient) external returns (uint256 id) {
        if (recipient == address(0)) revert ZeroRecipient();
        Listing storage l = listings[listingId];
        if (!l.active) revert ListingInactive();

        uint64 price = l.priceZat;
        bytes20 payeeHash = l.payeeHash;
        bool p2sh = l.payeeP2sh;
        uint32 window = l.window;

        bytes32 tagKey = keccak256(abi.encode(p2sh, payeeHash, price));
        uint64 tag = lastTag[tagKey] + 1;
        if (tag >= TAG_SPACE) revert TagsExhausted();
        lastTag[tagKey] = tag;

        uint64 nowAnchor = ZcashLib.anchorHeight();
        uint64 quoteZat = price + tag;
        id = ++reservationCount;
        reservations[id] = Reservation({
            recipient: recipient,
            quoteZat: quoteZat,
            minConf: l.minConf,
            payeeP2sh: p2sh,
            filled: false,
            payeeHash: payeeHash,
            reservedAt: nowAnchor,
            window: window
        });
        emit Reserved(id, listingId, recipient, quoteZat, payeeHash, p2sh, nowAnchor, nowAnchor + window);
    }

    /// @notice Deliver reservation `id` against Zcash payment (txid, vout).
    /// Anyone may call; the item always goes to the reservation's recipient.
    /// Succeeds iff the output pays exactly the quote to the reservation's
    /// payee script at >= minConf, was mined in (reservedAt, reservedAt +
    /// window], and this (txid, vout) has never filled a reservation.
    /// @param txid Display-order txid (as explorers print it).
    function claim(uint256 id, bytes32 txid, uint32 vout) external returns (uint256 itemId) {
        Reservation storage r = reservations[id];
        address recipient = r.recipient;
        if (recipient == address(0)) revert NoSuchReservation();
        if (r.filled) revert AlreadyFilled();

        bytes32 key = keccak256(abi.encode(txid, vout));
        uint256 used = paymentUsed[key];
        if (used != 0) revert PaymentAlreadyUsed(used);

        uint64 quoteZat = r.quoteZat;
        ZcashLib.Payment memory p =
            ZcashLib.requireOutputPays(txid, vout, _script(r.payeeHash, r.payeeP2sh), quoteZat, r.minConf);
        if (p.valueZat != quoteZat) revert WrongAmount(p.valueZat, quoteZat);
        uint64 reservedAt = r.reservedAt;
        if (p.height <= reservedAt) revert PaidBeforeReservation(p.height, reservedAt);
        uint64 dl = reservedAt + r.window;
        if (p.height > dl) revert PaidAfterDeadline(p.height, dl);

        r.filled = true;
        paymentUsed[key] = id;
        itemId = _deliver(recipient);
        emit Claimed(id, recipient, txid, vout, p.height, itemId);
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    /// @notice Exact amount (zatoshis) reservation `id` must pay.
    function quote(uint256 id) external view returns (uint64) {
        return reservations[id].quoteZat;
    }

    /// @notice The exact scriptPubKey reservation `id` must pay.
    function payeeScript(uint256 id) external view returns (bytes memory) {
        Reservation storage r = reservations[id];
        return _script(r.payeeHash, r.payeeP2sh);
    }

    /// @notice Last Zcash height at which reservation `id`'s payment may be mined.
    function deadline(uint256 id) external view returns (uint64) {
        Reservation storage r = reservations[id];
        return r.recipient == address(0) ? 0 : r.reservedAt + r.window;
    }

    // ------------------------------------------------------------------
    // Delivery
    // ------------------------------------------------------------------

    /// @dev Hand one item to `recipient` and return its id. Called once per
    /// filled reservation, after all state is written. Must not revert for
    /// recipient-side reasons: the buyer has already paid.
    function _deliver(address recipient) internal virtual returns (uint256 itemId);

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    function _set(
        uint256 id,
        uint64 priceZat,
        bytes20 payeeHash,
        bool payeeP2sh,
        uint32 window,
        uint16 minConf,
        bool active
    ) private {
        if (priceZat == 0 || priceZat % TAG_SPACE != 0 || priceZat > type(uint64).max - TAG_SPACE) {
            revert BadParams();
        }
        if (payeeHash == bytes20(0) || window == 0 || minConf == 0) revert BadParams();
        Listing storage l = listings[id];
        l.priceZat = priceZat;
        l.minConf = minConf;
        l.payeeP2sh = payeeP2sh;
        l.active = active;
        l.payeeHash = payeeHash;
        l.window = window;
        emit ListingUpdated(id, priceZat, payeeHash, payeeP2sh, window, minConf, active);
    }

    function _script(bytes20 h, bool p2sh) private pure returns (bytes memory) {
        return p2sh ? ZcashLib.p2sh(h) : ZcashLib.p2pkh(h);
    }
}

interface IAshwingsMint {
    function mint() external returns (uint256 id);
    function transferFrom(address from, address to, uint256 id) external;
}

/// @title AshwingsZecCheckout: mint an Ashwing by paying ZEC
/// @notice Delivery uses Ashwings' own public, free `mint()` (the checkout
/// mints to itself, then transfers), so Ashwings needs no minter role and
/// no change: this works against the deployed collection as-is. Plain
/// transferFrom, not safeTransferFrom: a paid claim must never fail on the
/// recipient side.
contract AshwingsZecCheckout is ZecCheckout {
    IAshwingsMint public immutable ashwings;

    constructor(address ashwings_) {
        ashwings = IAshwingsMint(ashwings_);
    }

    function _deliver(address recipient) internal override returns (uint256 id) {
        id = ashwings.mint();
        ashwings.transferFrom(address(this), recipient, id);
    }
}
