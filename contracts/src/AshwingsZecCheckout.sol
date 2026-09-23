// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ZecCheckout} from "./zcash/ZecCheckout.sol";
import {ZcashLib} from "./zcash/ZcashLib.sol";

/// @notice What the checkout needs from Ashwings: the supply ledger for
/// ZEC reservations, and a mint only this checkout may call.
interface IAshwingsZecMint {
    function holdForZec() external;
    function releaseZecHolds(uint256 n) external;
    function mintForZec(address to, bool fromHold) external returns (uint256 id);
}

/// @title AshwingsZecCheckout: the ZEC price of an Ashwing
/// @notice Created by the Ashwings constructor, which passes the ZEC terms.
/// It is a ZecCheckout with exactly one listing (#1), fixed forever: price,
/// payee t-address, window and minConf are set at deploy, and {list} and
/// {update} always revert. Its seller is this contract itself, which never
/// calls update. So the buyer flow and ABI are exactly ZecCheckout's
/// (reserve(1, recipient), pay the quote, claim(order, txid, vout)), and the
/// /ashwings/buy page and tools/checkout-relayer work unchanged.
///
/// Supply. Ashwings is capped. A reservation holds one unit of supply
/// until it is claimed, or until its hold expires: HOLD = window + minConf
/// + CLAIM_GRACE Zcash blocks after reservedAt. So a buyer who pays inside
/// the window and claims within the grace can never be sold out from under
/// them. Expired holds are released lazily by {sweep} (anyone may call it;
/// reserve and a sold-out SOVA mint call it). A claim after its hold was
/// released still works while supply is left, and reverts "ASHW: sold out"
/// once it is not (the ZEC reached the payee; a refund is off-chain).
///
/// Tags wrap. Unlike the base, tags cycle through [1, TAG_SPACE) instead of
/// running out, because the terms can never change to get fresh ones. This
/// keeps the base's guarantee that a payment inside its own window can fill
/// only its own reservation: every reservation made while R holds is either
/// still holding or already minted when R's hold ends, so at most
/// MAX_SUPPLY < TAG_SPACE - 1 of them exist, and R's tag cannot come round
/// again before R's hold ends. A later reservation with R's tag was made
/// after R's window closed, so a payment R could claim was mined before it
/// (PaidBeforeReservation). Only a late payment for R (already unclaimable
/// by R) can ever match a later order.
contract AshwingsZecCheckout is ZecCheckout {
    /// @notice Zcash blocks (~75 s each) a claim has after the window plus
    /// minConf before its supply hold may be released: 96 = about 2 h.
    uint64 public constant CLAIM_GRACE = 96;
    /// @notice Most reservations one {sweep} looks at (bounds its gas).
    uint256 public constant SWEEP_MAX = 32;
    /// @notice The single listing.
    uint256 public constant LISTING = 1;

    IAshwingsZecMint public immutable ashwings;
    /// @notice Zcash blocks after reservedAt that a reservation holds supply.
    uint64 public immutable holdBlocks;

    /// @notice Reservations 1..swept have had their hold settled (released
    /// by a sweep, or used by their claim first).
    uint256 public swept;

    error Immutable();

    event HoldsReleased(uint256 through, uint256 released);

    constructor(uint64 priceZat, bytes20 payeeHash, bool payeeP2sh, uint32 window, uint16 minConf) {
        ashwings = IAshwingsZecMint(msg.sender);
        holdBlocks = uint64(window) + minConf + CLAIM_GRACE;
        _open(address(this), priceZat, payeeHash, payeeP2sh, window, minConf);
    }

    /// @notice Always reverts: the one listing is fixed at deploy.
    function list(uint64, bytes20, bool, uint32, uint16) external pure override returns (uint256) {
        revert Immutable();
    }

    /// @notice Always reverts: the one listing is fixed at deploy.
    function update(uint256, uint64, bytes20, bool, uint32, uint16, bool) external pure override {
        revert Immutable();
    }

    /// @notice Release the supply held by expired, unclaimed reservations
    /// (at most SWEEP_MAX per call; call again for more). Anyone may call.
    /// @return released Holds released by this call.
    function sweep() public returns (uint256 released) {
        uint256 s = swept;
        uint256 n = reservationCount;
        if (s == n) return 0;
        uint256 end = n - s > SWEEP_MAX ? s + SWEEP_MAX : n;
        uint64 nowAnchor = ZcashLib.anchorHeight();
        uint64 hold = holdBlocks;
        // Anchors only grow and every hold is equally long, so holds expire
        // in reservation order: stop at the first live one.
        while (s < end) {
            Reservation storage r = reservations[s + 1];
            if (nowAnchor <= r.reservedAt + hold) break;
            if (!r.filled) released++;
            s++;
        }
        if (s == swept) return 0;
        swept = s;
        if (released != 0) ashwings.releaseZecHolds(released);
        emit HoldsReleased(s, released);
    }

    /// @notice Zcash anchor height after which reservation `id` stops
    /// holding supply (0 if there is no such reservation).
    function holdUntil(uint256 id) external view returns (uint64) {
        Reservation storage r = reservations[id];
        return r.recipient == address(0) ? 0 : r.reservedAt + holdBlocks;
    }

    // ------------------------------------------------------------------
    // ZecCheckout hooks
    // ------------------------------------------------------------------

    function _beforeReserve(uint256, uint64) internal override {
        sweep();
        ashwings.holdForZec(); // reverts "ASHW: sold out"
    }

    function _deliver(uint256 reservationId, address recipient) internal override returns (uint256) {
        // Past `swept`, this reservation's hold is still counted: use it.
        if (reservationId > swept) return ashwings.mintForZec(recipient, true);
        // Hold already released: mint from free supply, after freeing any
        // other expired holds.
        sweep();
        return ashwings.mintForZec(recipient, false);
    }

    function _nextTag(bytes32 tagKey) internal override returns (uint64 tag) {
        tag = lastTag[tagKey] + 1;
        if (tag >= TAG_SPACE) tag = 1;
        lastTag[tagKey] = tag;
    }
}
