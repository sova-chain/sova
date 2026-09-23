// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ZcashLib} from "./ZcashLib.sol";

/// @title ZecEscrow: custody-free ZEC -> SOVA trades (SIP-4 use case 1, D1)
/// @notice A maker locks SOVA and names a price in zatoshis and a fresh
/// transparent Zcash address (P2PKH) of their own. A taker reserves the
/// order with a bond, pays the ZEC on Zcash straight to the maker's
/// address from any wallet, and claims the SOVA by pointing at the
/// payment. The contract reads the payment through the SIP-4 precompile.
/// Nobody holds anybody's ZEC: it goes from the taker to the maker on
/// Zcash and never leaves Zcash.
///
/// Clock. Time is the Zcash anchor height E_N (ZcashLib.anchorHeight),
/// which advances one per Sova block. A reservation made at anchor A is
/// live while E_N <= A + window. A claim needs the payment mined at a
/// height > A (strictly after the reservation) with >= minConf
/// confirmations, which together put it inside the window.
///
/// Rules. No admin, no owner, no upgrade, no fees. One order per maker
/// address at a time (an address is "live" from create until fill or
/// cancel). One (txid, vout) fills at most one order, ever.
///
/// Honest limits:
/// - Free option. During the window the taker can watch the price and
///   simply not pay; they lose only the bond. Makers limit this with the
///   bond size and a short window.
/// - Pay promptly. The taker must get the payment to minConf before the
///   window closes. A payment that confirms late is lost to the taker (the
///   maker has the ZEC, the bond is forfeited), and if the order reopens,
///   the next reserver can claim that late payment. Never pay after your
///   window has closed.
/// - Transparent maker address. The maker's receiving address and the
///   amount are public on Zcash. Use a fresh address per order and sweep
///   it to shielded afterwards. The taker can pay from a shielded balance
///   (z->t), so the payer's side keeps its privacy up to that output.
/// - Reorgs. A Zcash reorg deeper than the payment's confirmations can
///   remove the payment. SIP-4 reorgs Sova with Zcash, so a claim resting
///   on a removed payment is rolled back too; what cannot be rolled back
///   is anything done outside Sova meanwhile with the released SOVA.
///   minConf is the maker's lever: testnet 3, mainnet 10, more for size.
/// - Uniqueness is per contract. Reusing a maker address across other
///   contracts or orders elsewhere is the maker's risk.
contract ZecEscrow {
    enum Status {
        None,
        Open,
        Filled,
        Cancelled
    }

    struct Order {
        // slot 0
        address maker;
        uint64 priceZat;
        uint32 minConf;
        // slot 1
        bytes20 makerPkh;
        uint64 window;
        Status status;
        // slot 2
        uint128 amount;
        uint128 bond;
        // slot 3: reservation (taker == 0 means none)
        address taker;
        uint64 reservedAt;
    }

    /// @notice Orders by id; ids start at 1.
    mapping(uint256 => Order) public orders;
    uint256 public orderCount;

    /// @notice True while an Open order uses this maker pubkey hash.
    mapping(bytes20 => bool) public pkhLive;

    /// @notice keccak256(abi.encode(txid, vout)) => order it filled (0 = unused).
    mapping(bytes32 => uint256) public paymentFilled;

    event OrderCreated(
        uint256 indexed id,
        address indexed maker,
        uint256 amount,
        uint64 priceZat,
        bytes20 makerPkh,
        uint256 bond,
        uint64 window,
        uint32 minConf
    );
    event Reserved(uint256 indexed id, address indexed taker, uint64 reservedAt, uint64 deadline);
    event Claimed(uint256 indexed id, address indexed taker, bytes32 txid, uint32 vout, uint64 valueZat, uint64 height);
    event ReservationExpired(uint256 indexed id, address indexed taker, uint256 bondToMaker);
    event Cancelled(uint256 indexed id);

    error BadParams();
    error PkhInUse();
    error NotOpen();
    error AlreadyReserved();
    error NotReserved();
    error WrongBond();
    error ReservationOver();
    error ReservationLive();
    error PaymentAlreadyUsed(uint256 filledOrder);
    error PaidBeforeReservation(uint64 paymentHeight, uint64 reservedAt);
    error NotMaker();
    error TransferFailed();

    // ------------------------------------------------------------------
    // Maker
    // ------------------------------------------------------------------

    /// @notice Lock msg.value SOVA for sale at `priceZat` zatoshis, paid to
    /// the P2PKH address with hash `makerPkh` (fresh per order).
    /// @param bond SOVA (wei) a taker posts to reserve; forfeited to the
    /// maker if the window lapses unclaimed. 0 is allowed (no griefing cost).
    /// @param window Reservation length in Zcash blocks (~75 s each); must be
    /// >= minConf so a prompt payment can reach depth in time.
    /// @param minConf Required payment depth (>= 1). Testnet 3, mainnet 10.
    function createOrder(uint64 priceZat, bytes20 makerPkh, uint128 bond, uint64 window, uint32 minConf)
        external
        payable
        returns (uint256 id)
    {
        if (msg.value == 0 || msg.value > type(uint128).max) revert BadParams();
        if (priceZat == 0 || makerPkh == bytes20(0) || minConf == 0) revert BadParams();
        if (window < minConf || window > type(uint32).max) revert BadParams();
        if (pkhLive[makerPkh]) revert PkhInUse();
        pkhLive[makerPkh] = true;

        id = ++orderCount;
        orders[id] = Order({
            maker: msg.sender,
            priceZat: priceZat,
            minConf: minConf,
            makerPkh: makerPkh,
            window: window,
            status: Status.Open,
            amount: uint128(msg.value),
            bond: bond,
            taker: address(0),
            reservedAt: 0
        });
        emit OrderCreated(id, msg.sender, msg.value, priceZat, makerPkh, bond, window, minConf);
    }

    /// @notice Maker takes the order down (SOVA back, plus any forfeited
    /// bond). Not while a reservation is live.
    function cancel(uint256 id) external {
        Order storage o = orders[id];
        if (o.maker != msg.sender) revert NotMaker();
        if (o.status != Status.Open) revert NotOpen();
        uint256 forfeited;
        if (o.taker != address(0)) {
            if (_live(o)) revert ReservationLive();
            forfeited = o.bond;
            emit ReservationExpired(id, o.taker, forfeited);
            o.taker = address(0);
            o.reservedAt = 0;
        }
        o.status = Status.Cancelled;
        pkhLive[o.makerPkh] = false;
        emit Cancelled(id);
        _send(msg.sender, uint256(o.amount) + forfeited);
    }

    // ------------------------------------------------------------------
    // Taker
    // ------------------------------------------------------------------

    /// @notice Reserve an open order by posting exactly its bond. Starts the
    /// window at the current anchor. Pay ZEC only AFTER this is mined, and
    /// promptly. An expired reservation is settled first (bond to maker).
    function reserve(uint256 id) external payable {
        Order storage o = orders[id];
        if (o.status != Status.Open) revert NotOpen();
        if (msg.value != o.bond) revert WrongBond();

        address prevTaker = o.taker;
        if (prevTaker != address(0) && _live(o)) revert AlreadyReserved();

        uint64 nowAnchor = ZcashLib.anchorHeight();
        o.taker = msg.sender;
        o.reservedAt = nowAnchor;
        emit Reserved(id, msg.sender, nowAnchor, nowAnchor + o.window);

        if (prevTaker != address(0)) {
            emit ReservationExpired(id, prevTaker, o.bond);
            _send(o.maker, o.bond);
        }
    }

    /// @notice Fill a reserved order with Zcash payment (txid, vout). Anyone
    /// may call; SOVA and the bond always go to the reserving taker.
    /// Succeeds iff the reservation is live, the output pays >= priceZat to
    /// the maker's P2PKH script at >= minConf, it was mined after the
    /// reservation, and this (txid, vout) has never filled an order.
    /// @param txid Display-order txid (as explorers print it).
    function claim(uint256 id, bytes32 txid, uint32 vout) external {
        Order storage o = orders[id];
        if (o.status != Status.Open) revert NotOpen();
        address taker = o.taker;
        if (taker == address(0)) revert NotReserved();
        if (!_live(o)) revert ReservationOver();

        bytes32 key = keccak256(abi.encode(txid, vout));
        uint256 used = paymentFilled[key];
        if (used != 0) revert PaymentAlreadyUsed(used);

        ZcashLib.Payment memory p =
            ZcashLib.requireOutputPays(txid, vout, ZcashLib.p2pkh(o.makerPkh), o.priceZat, o.minConf);
        if (p.height <= o.reservedAt) revert PaidBeforeReservation(p.height, o.reservedAt);

        paymentFilled[key] = id;
        o.status = Status.Filled;
        pkhLive[o.makerPkh] = false;
        emit Claimed(id, taker, txid, vout, p.valueZat, p.height);
        _send(taker, uint256(o.amount) + o.bond);
    }

    /// @notice Settle a lapsed reservation: bond to the maker, order reopens.
    /// Anyone may call. (reserve and cancel also do this implicitly.)
    function expire(uint256 id) external {
        Order storage o = orders[id];
        if (o.status != Status.Open) revert NotOpen();
        address taker = o.taker;
        if (taker == address(0)) revert NotReserved();
        if (_live(o)) revert ReservationLive();
        o.taker = address(0);
        o.reservedAt = 0;
        emit ReservationExpired(id, taker, o.bond);
        _send(o.maker, o.bond);
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    /// @notice The exact scriptPubKey the taker must pay.
    function makerScript(uint256 id) external view returns (bytes memory) {
        return ZcashLib.p2pkh(orders[id].makerPkh);
    }

    /// @notice Last anchor height at which the current reservation can
    /// still be claimed (0 if unreserved).
    function deadline(uint256 id) external view returns (uint64) {
        Order storage o = orders[id];
        return o.taker == address(0) ? 0 : o.reservedAt + o.window;
    }

    /// @notice True iff the order is Open with a reservation that has not lapsed.
    function reservationLive(uint256 id) external view returns (bool) {
        Order storage o = orders[id];
        return o.status == Status.Open && o.taker != address(0) && _live(o);
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    function _live(Order storage o) private view returns (bool) {
        return ZcashLib.anchorHeight() <= o.reservedAt + o.window;
    }

    function _send(address to, uint256 value) private {
        if (value == 0) return;
        (bool ok,) = to.call{value: value}("");
        if (!ok) revert TransferFailed();
    }
}
