// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ZcashLib} from "./ZcashLib.sol";

/// @title ZcashTrigger: turn a Zcash condition into a Sova action (SIP-7 §4.3)
/// @notice Zcash cannot call a Sova contract, so someone has to send the
/// transaction. This base pays whoever does it first: anyone calls
/// `poke(h)` with a Zcash height `h` they claim satisfies the condition;
/// the contract re-checks everything itself in O(1), acts once, and pays
/// the caller `bounty` (native SOVA) from its own balance. A failed check
/// reverts, so spam costs the spammer. No owner, no admin, no withdraw:
/// the only ways value leaves are `_act` and the bounty.
///
/// The caller supplies the witness because contracts cannot search Zcash.
/// Good conditions are ones where a single height proves the claim, for
/// example "the pool crossed X at h" (`value(h) >= X && value(h-1) < X`).
/// Keepers find `h` off chain (the node's `sova_subscribe("zcashBlocks")`
/// feed, `ZcashBlocks.publish` logs, or zebrad) and can dry-run with
/// `ready(h)` or an `eth_call` of `poke(h)`.
///
/// Depth: `h` must have at least `minConf` confirmations at the current
/// anchor (h + minConf - 1 <= E_N), SIP-4's depth rule. A Zcash reorg
/// below that depth reorgs Sova with it, so the trigger's own state stays
/// consistent either way; minConf protects what happens off Sova.
///
/// Funding: send SOVA to the contract (constructor value or plain
/// transfer). If it holds less than `bounty` when fired, the caller gets
/// what is left after `_act`.
///
/// One-shot by default. A repeating trigger overrides `_armed` (for
/// example "last fired height + cooldown < h") and keeps its own state.
abstract contract ZcashTrigger {
    /// @notice Paid to the first valid caller of `poke`.
    uint256 public immutable bounty;
    /// @notice Confirmations `h` needs at the current anchor (>= 1).
    uint64 public immutable minConf;
    /// @notice Earliest witness height accepted (so a condition that was
    /// already true before deployment cannot fire on an old block).
    uint64 public immutable startHeight;

    /// @notice Times fired, and the witness height of the last firing.
    uint64 public fires;
    uint64 public lastFiredAt;

    uint256 private _lock = 1;

    event Fired(uint64 indexed height, address indexed keeper, uint256 bountyPaid);

    error MinConfZero();
    error NotArmed(uint64 h);
    error TooEarly(uint64 h, uint64 startHeight);
    error TooShallow(uint64 h, uint64 anchorHeight, uint64 minConf);
    error ConditionNotMet(uint64 h);
    error BountyFailed();
    error Reentrant();

    constructor(uint256 bounty_, uint64 minConf_, uint64 startHeight_) payable {
        if (minConf_ == 0) revert MinConfZero();
        bounty = bounty_;
        minConf = minConf_;
        startHeight = startHeight_;
    }

    receive() external payable {}

    /// @notice Fire the trigger with witness height `h`. Anyone can call.
    function poke(uint64 h) external {
        if (_lock != 1) revert Reentrant();
        _lock = 2;
        if (!_armed(h)) revert NotArmed(h);
        if (h < startHeight) revert TooEarly(h, startHeight);
        uint64 e = ZcashLib.anchorHeight();
        if (h + minConf - 1 > e) revert TooShallow(h, e, minConf);
        if (!condition(h)) revert ConditionNotMet(h);

        fires += 1;
        lastFiredAt = h;
        _act(h);

        uint256 pay = address(this).balance < bounty ? address(this).balance : bounty;
        if (pay > 0) {
            (bool ok,) = msg.sender.call{value: pay}("");
            if (!ok) revert BountyFailed();
        }
        emit Fired(h, msg.sender, pay);
        _lock = 1;
    }

    /// @notice True iff `poke(h)` would pass every check now (it can still
    /// fail on the bounty transfer to a contract that rejects SOVA).
    function ready(uint64 h) external view returns (bool) {
        if (_lock != 1 || !_armed(h) || h < startHeight) return false;
        if (h + minConf - 1 > ZcashLib.anchorHeight()) return false;
        return condition(h);
    }

    /// @notice One-shot: armed until the first firing. Override to repeat.
    function _armed(uint64) internal view virtual returns (bool) {
        return fires == 0;
    }

    /// @notice The Zcash condition, checked at witness height `h`. Must be
    /// a pure function of Zcash state at `h` (and before) plus this
    /// contract's own state, so every node agrees.
    function condition(uint64 h) public view virtual returns (bool);

    /// @notice What happens when it fires (runs before the bounty is paid,
    /// after `fires`/`lastFiredAt` are updated).
    function _act(uint64 h) internal virtual;
}

/// @title ShieldedGrowthPrize: pay a beneficiary when Zcash's shielded pool grows past a line
/// @notice Example ZcashTrigger. Fires once, at the Zcash height where the
/// shielded total (Sprout + Sapling + Orchard + Ironwood) first reaches
/// `threshold` zatoshis, provided that height is >= `startHeight`. Then it
/// sends everything it holds except the keeper's bounty to
/// `beneficiary`. Reads the SIP-4/SIP-7 precompile (full history from the
/// epoch base), not the ZcashBlocks ring.
contract ShieldedGrowthPrize is ZcashTrigger {
    uint64 public immutable threshold;
    address public immutable beneficiary;

    event Prize(address indexed beneficiary, uint256 amount, uint64 shieldedZat);

    constructor(uint64 threshold_, address beneficiary_, uint256 bounty_, uint64 minConf_, uint64 startHeight_)
        payable
        ZcashTrigger(bounty_, minConf_, startHeight_)
    {
        threshold = threshold_;
        beneficiary = beneficiary_;
    }

    function condition(uint64 h) public view override returns (bool) {
        return ZcashLib.shieldedCrossedAbove(ZcashLib.POOLS, h, threshold);
    }

    function _act(uint64 h) internal override {
        uint256 bal = address(this).balance;
        uint256 amount = bal > bounty ? bal - bounty : 0;
        (, uint64 total) = ZcashLib.shieldedTotal(h);
        if (amount > 0) {
            (bool ok,) = beneficiary.call{value: amount}("");
            require(ok, "ShieldedGrowthPrize: payout");
        }
        emit Prize(beneficiary, amount, total);
    }
}
