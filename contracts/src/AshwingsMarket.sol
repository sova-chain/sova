// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

interface IAshwingsForMarket {
    function ownerOf(uint256 id) external view returns (address);
    function getApproved(uint256 id) external view returns (address);
    function transferFrom(address from, address to, uint256 id) external;
}

/// @title AshwingsMarket: list, cancel, buy Ashwings for SOVA
/// @notice No owner, no admin, no setters, no upgrade. The fee (feeBps of
/// each sale: 100 = 1%) and the treasury it goes to are fixed at deploy;
/// the treasury only receives, it has no power over this contract.
///
/// Listings are non-custodial: the owl stays in the seller's wallet until
/// it sells. To list, approve this market for that one owl
/// (`Ashwings.approve(market, id)`), then {list}. A listing can be bought
/// only while the seller still owns the owl AND the market is still its
/// approved address. Ashwings clears a token's approval on every transfer,
/// so a listing dies when the owl moves and does not come back if the owl
/// later returns to the seller; approving someone else kills it too.
/// Operator approval (setApprovalForAll) is deliberately not enough, since
/// it survives transfers.
///
/// Fee math: fee = price * feeBps / 10,000, rounded down (at 1%:
/// floor(price / 100)); the seller gets price - fee.
/// The two always add up to the price, so no wei is created or stuck.
/// Fees collect here and anyone may {withdrawFees} them to the treasury,
/// so a treasury that cannot receive never blocks a sale.
///
/// Reentrancy: every state-changing entry point takes one lock, and
/// {buy} deletes the listing and books the fee before any external call.
contract AshwingsMarket {
    /// @notice Highest fee a deploy may set: 10%.
    uint16 public constant MAX_FEE_BPS = 1_000;

    // Private immutables with same-named getters, so the constructor
    // parameters can carry the getter names (deploy-kit.sh matches args by
    // name and reads each back).
    IAshwingsForMarket private immutable _ashwings;
    address private immutable _treasury;
    uint16 private immutable _feeBps;

    struct Listing {
        address seller;
        uint96 price; // wei; 0 = not listed
    }

    /// @notice Listing by token id. May be stale; see {isLive}.
    mapping(uint256 => Listing) public listings;
    /// @notice Fees booked and not yet sent to the treasury.
    uint256 public feesOwed;

    uint256 private _lock = 1;

    event Listed(uint256 indexed id, address indexed seller, uint256 price);
    event Canceled(uint256 indexed id, address indexed seller);
    event Sold(uint256 indexed id, address indexed seller, address indexed buyer, uint256 price, uint256 fee);
    event FeesWithdrawn(address indexed treasury, uint256 amount);

    error NotOwner();
    error NotApproved();
    error BadPrice();
    error NotListed();
    error NotSeller();
    error WrongValue(uint256 sent, uint256 price);
    error StaleListing();
    error PaymentFailed();
    error Reentrancy();

    modifier locked() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    /// @param ashwings The Ashwings collection.
    /// @param treasury Receives the fees (and nothing else).
    /// @param feeBps Fee per sale in basis points, <= 1,000; Rob: 100 (1%).
    constructor(address ashwings, address treasury, uint16 feeBps) {
        require(ashwings != address(0) && treasury != address(0) && feeBps <= MAX_FEE_BPS, "MARKET: bad params");
        _ashwings = IAshwingsForMarket(ashwings);
        _treasury = treasury;
        _feeBps = feeBps;
    }

    function ashwings() external view returns (address) {
        return address(_ashwings);
    }

    function treasury() external view returns (address) {
        return _treasury;
    }

    /// @notice Fee per sale in basis points (100 = 1%).
    function feeBps() external view returns (uint16) {
        return _feeBps;
    }

    /// @notice List owl `id` at `price` wei (replaces any earlier listing
    /// of it). You must own it and have approved this market for it.
    function list(uint256 id, uint256 price) external locked {
        if (price == 0 || price > type(uint96).max) revert BadPrice();
        if (_ashwings.ownerOf(id) != msg.sender) revert NotOwner();
        if (_ashwings.getApproved(id) != address(this)) revert NotApproved();
        listings[id] = Listing(msg.sender, uint96(price));
        emit Listed(id, msg.sender, price);
    }

    /// @notice Remove a listing. The seller may always; anyone may remove a
    /// stale one (owl moved or approval gone), which keeps indexes clean.
    function cancel(uint256 id) external locked {
        Listing memory l = listings[id];
        if (l.price == 0) revert NotListed();
        if (msg.sender != l.seller && _live(id, l.seller)) revert NotSeller();
        delete listings[id];
        emit Canceled(id, l.seller);
    }

    /// @notice Buy owl `id`. Send exactly its listed price: a listing that
    /// changed after you looked reverts rather than charging you more.
    function buy(uint256 id) external payable locked {
        Listing memory l = listings[id];
        if (l.price == 0) revert NotListed();
        if (msg.value != l.price) revert WrongValue(msg.value, l.price);
        if (!_live(id, l.seller)) revert StaleListing();

        delete listings[id];
        uint256 fee = feeOf(msg.value);
        feesOwed += fee;

        // Plain transferFrom: no receiver callback into the buyer.
        _ashwings.transferFrom(l.seller, msg.sender, id);
        (bool ok,) = l.seller.call{value: msg.value - fee}("");
        if (!ok) revert PaymentFailed();
        emit Sold(id, l.seller, msg.sender, msg.value, fee);
    }

    /// @notice Send all booked fees to the treasury. Anyone may call.
    function withdrawFees() external locked {
        uint256 amount = feesOwed;
        feesOwed = 0;
        (bool ok,) = _treasury.call{value: amount}("");
        if (!ok) revert PaymentFailed();
        emit FeesWithdrawn(_treasury, amount);
    }

    /// @notice True if owl `id` is listed and can be bought right now.
    function isLive(uint256 id) external view returns (bool) {
        Listing memory l = listings[id];
        return l.price != 0 && _live(id, l.seller);
    }

    /// @notice The fee on a sale at `price`: floor(price * feeBps / 10,000).
    function feeOf(uint256 price) public view returns (uint256) {
        return price * _feeBps / 10_000;
    }

    function _live(uint256 id, address seller) internal view returns (bool) {
        return _ashwings.ownerOf(id) == seller && _ashwings.getApproved(id) == address(this);
    }
}
