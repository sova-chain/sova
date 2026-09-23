// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {AshwingsZecCheckout} from "./AshwingsZecCheckout.sol";
import {ZcashAddress} from "./zcash/ZcashAddress.sol";

/// @title Ashwings
/// @notice Day-one native mint: fully on-chain generative pixel owls
/// (Sova = owl). 10,000 ever, at one fixed price, payable two ways:
/// - in SOVA on Sova: {mint}, exactly `priceWei`, to `treasury`;
/// - in ZEC on Zcash: the {zecCheckout} this contract creates (a
///   ZecCheckout with one fixed listing). Reserve, pay the exact quote to
///   the ZEC payee from any Zcash wallet, then claim with the txid; Sova
///   reads the payment through the SIP-4 precompile. No bridge.
/// Every wei of SOVA traces back to destroyed ZEC. "Minted with destroyed
/// money" is literal.
///
/// No owner, no admin, no setters, no metadata server: both prices, both
/// payees and the cap are fixed at deploy (the treasury only receives), and the SVG is generated and
/// stored by the EVM itself, so the artifact lives exactly as long as the
/// chain does. SOVA paid for mints collects here; anyone may {withdraw} it,
/// and it can only go to `treasury`.
///
/// Supply: totalSupply + zecHeld <= MAX_SUPPLY. `zecHeld` counts open ZEC
/// reservations, so a buyer who has reserved and paid in time always gets
/// an owl (see AshwingsZecCheckout for how holds expire).
///
/// Art spec (design lab: contracts/design/ashwing24.py, the approved
/// 24x24 redraw): a 24-row x 12-column left half is drawn and mirrored;
/// tufts, speckle, beak, pupils, closed eyes, the void-iris catchlight,
/// burn embers and accessories go on after the mirror, and pupils always
/// share one glance so both eyes look the same way (owls can't cross
/// their eyes). Ten traits, one seed byte each, weights out of 256.
/// Eleven species palettes, gold tier common, spectral rarest. The SVG is
/// one background rect plus row run-length rects, byte-for-byte equal to
/// the generator's output (contracts/test/AshwingsParity.t.sol).
contract Ashwings {
    string public constant name = "Ashwings";
    string public constant symbol = "ASHW";
    uint256 public constant MAX_SUPPLY = 10_000;

    event Transfer(address indexed from, address indexed to, uint256 indexed id);
    event Approval(address indexed owner, address indexed spender, uint256 indexed id);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);
    event Withdrawn(address indexed treasury, uint256 amount);

    // Storage layout: slots 0..5 are the original collection's, in order
    // (the parity test plants seeds at slot 5). New state goes after.
    uint256 public totalSupply;
    mapping(uint256 => address) public ownerOf;
    mapping(address => uint256) public balanceOf;
    mapping(uint256 => address) public getApproved;
    mapping(address => mapping(address => bool)) public isApprovedForAll;
    mapping(uint256 => bytes32) public seedOf;
    /// @notice Supply held for open ZEC reservations (not yet claimed or expired).
    uint256 public zecHeld;
    /// @notice The Zcash t-address ZEC is paid to, as given at deploy.
    string public zecPayee;

    /// @notice Zcash blocks a ZEC quote holds: 40 = ~50 min.
    uint32 public constant ZEC_WINDOW = 40;
    /// @notice Payment depth before a ZEC claim, by the payee's network:
    /// 10 on mainnet (~12.5 min), 3 on testnet / regtest.
    uint16 public constant ZEC_MINCONF_MAINNET = 10;
    uint16 public constant ZEC_MINCONF_TESTNET = 3;

    // Getters below are functions (not public immutables) so the
    // constructor parameters can carry the getter names: the deploy kit
    // (contracts/script/deploy-kit.sh) matches args by name and reads each
    // one back through its same-named getter.
    uint256 private immutable _priceWei;
    uint256 private immutable _priceZat;
    address private immutable _treasury;
    AshwingsZecCheckout private immutable _zecCheckout;

    /// @param treasury Sova address that receives the SOVA (only receives:
    /// it has no power over this contract).
    /// @param zecPayee_ Zcash transparent address the ZEC is paid to: t1/t3
    /// (mainnet, minConf 10) or tm/t2 (testnet and regtest, minConf 3).
    /// @param priceWei SOVA price per owl, in wei.
    /// @param priceZat ZEC price per owl, in zatoshis: a positive multiple
    /// of 100,000 (0.001 ZEC); each order pays it plus a 1..99,999 zat tag.
    constructor(address treasury, string memory zecPayee_, uint256 priceWei, uint256 priceZat) {
        require(treasury != address(0), "ASHW: zero treasury");
        require(priceZat <= type(uint64).max, "ASHW: priceZat too large");
        (bytes20 payeeHash, bool p2sh, bool mainnet) = ZcashAddress.decode(zecPayee_);
        _treasury = treasury;
        _priceWei = priceWei;
        _priceZat = priceZat;
        zecPayee = zecPayee_;
        _zecCheckout = new AshwingsZecCheckout(
            uint64(priceZat), payeeHash, p2sh, ZEC_WINDOW, mainnet ? ZEC_MINCONF_MAINNET : ZEC_MINCONF_TESTNET
        );
    }

    /// @notice SOVA price of one mint, in wei.
    function priceWei() public view returns (uint256) {
        return _priceWei;
    }

    /// @notice ZEC price of one mint, in zatoshis (plus a per-order tag).
    function priceZat() external view returns (uint256) {
        return _priceZat;
    }

    /// @notice Receives SOVA mint payments (via {withdraw}). Nothing else.
    function treasury() public view returns (address) {
        return _treasury;
    }

    /// @notice The ZEC mint: reserve(1, you), pay, claim.
    function zecCheckout() public view returns (AshwingsZecCheckout) {
        return _zecCheckout;
    }

    /// @notice Mint one owl to yourself for exactly `priceWei` SOVA.
    function mint() external payable returns (uint256 id) {
        require(msg.value == _priceWei, "ASHW: wrong price");
        if (totalSupply + zecHeld >= MAX_SUPPLY) {
            // Expired ZEC holds may be all that is left: free them first.
            if (zecHeld != 0) _zecCheckout.sweep();
            require(totalSupply + zecHeld < MAX_SUPPLY, "ASHW: sold out");
        }
        id = _mint(msg.sender);
    }

    /// @notice Send all collected SOVA to `treasury`. Anyone may call.
    function withdraw() external {
        uint256 amount = address(this).balance;
        (bool ok,) = _treasury.call{value: amount}("");
        require(ok, "ASHW: withdraw failed");
        emit Withdrawn(_treasury, amount);
    }

    // ---------------------------------------------------------------
    // ZEC checkout hooks (callable by zecCheckout only)
    // ---------------------------------------------------------------

    modifier onlyZecCheckout() {
        require(msg.sender == address(_zecCheckout), "ASHW: not the checkout");
        _;
    }

    /// @notice Hold one unit of supply for a new ZEC reservation.
    function holdForZec() external onlyZecCheckout {
        require(totalSupply + zecHeld < MAX_SUPPLY, "ASHW: sold out");
        zecHeld++;
    }

    /// @notice Release `n` holds of expired, unclaimed reservations.
    function releaseZecHolds(uint256 n) external onlyZecCheckout {
        zecHeld -= n;
    }

    /// @notice Mint for a paid ZEC reservation, from its hold if it still
    /// has one, else from free supply.
    function mintForZec(address to, bool fromHold) external onlyZecCheckout returns (uint256) {
        if (fromHold) zecHeld--;
        else require(totalSupply + zecHeld < MAX_SUPPLY, "ASHW: sold out");
        return _mint(to);
    }

    function _mint(address to) internal returns (uint256 id) {
        id = ++totalSupply;
        seedOf[id] = keccak256(abi.encodePacked(to, id, block.prevrandao, blockhash(block.number - 1)));
        ownerOf[id] = to;
        unchecked {
            balanceOf[to]++;
        }
        emit Transfer(address(0), to, id);
    }

    // ---------------------------------------------------------------
    // ERC-721 transfer surface
    // ---------------------------------------------------------------

    function approve(address spender, uint256 id) external {
        address owner = ownerOf[id];
        require(msg.sender == owner || isApprovedForAll[owner][msg.sender], "ASHW: not authorized");
        getApproved[id] = spender;
        emit Approval(owner, spender, id);
    }

    function setApprovalForAll(address operator, bool approved) external {
        isApprovedForAll[msg.sender][operator] = approved;
        emit ApprovalForAll(msg.sender, operator, approved);
    }

    function transferFrom(address from, address to, uint256 id) public {
        require(from == ownerOf[id], "ASHW: wrong from");
        require(to != address(0), "ASHW: zero to");
        require(
            msg.sender == from || isApprovedForAll[from][msg.sender] || msg.sender == getApproved[id],
            "ASHW: not authorized"
        );
        unchecked {
            balanceOf[from]--;
            balanceOf[to]++;
        }
        ownerOf[id] = to;
        delete getApproved[id];
        emit Transfer(from, to, id);
    }

    function safeTransferFrom(address from, address to, uint256 id) external {
        safeTransferFrom(from, to, id, "");
    }

    function safeTransferFrom(address from, address to, uint256 id, bytes memory data) public {
        transferFrom(from, to, id);
        require(
            to.code.length == 0
                || IERC721Receiver(to).onERC721Received(msg.sender, from, id, data)
                    == IERC721Receiver.onERC721Received.selector,
            "ASHW: unsafe recipient"
        );
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == 0x01ffc9a7 || interfaceId == 0x80ac58cd || interfaceId == 0x5b5e139f;
    }

    // ---------------------------------------------------------------
    // Trait derivation. The spec is the design lab generator,
    // contracts/design/ashwing24.py; every SVG this contract renders is
    // byte-for-byte identical to it (contracts/test/AshwingsParity.t.sol
    // checks 155 golden seeds, contracts/test/fixtures/ashwings24/).
    //
    // One seed byte per trait, each read against a weight table that
    // sums to exactly 256, so uint8(seed[b]) is an unbiased roll:
    //   byte 0 species | 1 background | 2 tufts | 3 eye state | 4 glance
    //   byte 5 pupil   | 6 iris       | 7 beak  | 8 chest     | 9 accessory
    // The speckle chest reads the low 140 bits of the seed (2 per cell).
    // ---------------------------------------------------------------

    struct Traits {
        uint8 species; // 0 classic, 1 bright, 2 bronze, 3 barn, 4 moss, 5 ember, 6 dusk, 7 slate, 8 snowy, 9 midnight, 10 spectral
        uint8 background; // 0 ash, 1 slate, 2 sage, 3 cream, 4 clay, 5 night, 6 gold, 7 burn
        uint8 tufts; // 0 none, 1 tufts, 2 horns, 3 tall, 4 crest, 5 wild
        uint8 eyes; // 0 open, 1 sleepy, 2 zen, 3 wink
        int8 dx; // glance: -1 left, 0 centre, +1 right (both eyes: owls can't cross their eyes)
        uint8 pupil; // 0 round, 1 tall
        uint8 iris; // 0 natural, 1 amber, 2 ember, 3 void
        uint8 beak; // 0 small, 1 long, 2 hooked, 3 hoot
        uint8 chest; // 0 speckle, 1 bars, 2 chevrons, 3 bib
        uint8 accessory; // 0 none, 1 gold chain, 2 headband, 3 bow tie, 4 monocle, 5 earring, 6 pipe, 7 halo
    }

    // Weights out of 256, one byte per value.
    bytes private constant SPECIES_W = hex"331f1f1a1a1414140f0b05";
    bytes private constant BACKGROUND_W = hex"3c3228241e160c06";
    bytes private constant TUFTS_W = hex"464632241608";
    bytes private constant EYES_W = hex"ba26120e";
    bytes private constant GLANCE_W = hex"26b426";
    bytes private constant PUPIL_W = hex"a65a";
    bytes private constant IRIS_W = hex"b42c160a";
    bytes private constant BEAK_W = hex"80501e12";
    bytes private constant CHEST_W = hex"644c321e";
    bytes private constant ACCESSORY_W = hex"781c161614141008";

    function traitsOf(uint256 id) public view returns (Traits memory) {
        require(ownerOf[id] != address(0), "ASHW: unminted");
        return _traits(seedOf[id]);
    }

    /// First index i with roll < w[0] + ... + w[i]. Tables sum to 256, so
    /// every roll lands.
    function _pick(bytes1 roll, bytes memory w) internal pure returns (uint8 i) {
        uint256 r = uint8(roll);
        uint256 acc;
        while (true) {
            acc += uint8(w[i]);
            if (r < acc) return i;
            i++;
        }
    }

    function _traits(bytes32 seed) internal pure returns (Traits memory t) {
        t.species = _pick(seed[0], SPECIES_W);
        t.background = _pick(seed[1], BACKGROUND_W);
        t.tufts = _pick(seed[2], TUFTS_W);
        t.eyes = _pick(seed[3], EYES_W);
        t.dx = int8(_pick(seed[4], GLANCE_W)) - 1;
        t.pupil = _pick(seed[5], PUPIL_W);
        t.iris = _pick(seed[6], IRIS_W);
        t.beak = _pick(seed[7], BEAK_W);
        t.chest = _pick(seed[8], CHEST_W);
        t.accessory = _pick(seed[9], ACCESSORY_W);
    }

    // ---------------------------------------------------------------
    // Art tables. Symbols:
    //   .  background        O  outline / pupil      B  body
    //   D  rim / shade       W  wing                 L  facial disc
    //   E  eye (iris)        N  beak                 C  chest zone
    //   K  gold  Y  deep gold  R  red  r  dark red  T  pipe wood
    //   Q  ember  S  smoke  H  cream highlight (halo glint, void catchlight)
    //   G  halo gold (gold; deep gold on the gold background)
    // ---------------------------------------------------------------

    /// The owl's left half as the viewer sees it: 24 rows x 12 columns,
    /// mirrored into the 24x24 canvas.
    bytes private constant HALF = "............" "............" "............" "............" "......OOOOOO"
        "....OODDDDDD" "...ODBBBBBBD" "...ODBLLLLBD" "...ODLLEELLD" "...ODLEEEELL" "...ODLEEEELB" "...ODLLEELLB"
        "...ODLLLLLLB" "...ODBLLLLLL" "...ODBBBLLLL" "....ODBBBBBB" "...OWDBBBBBB" "..OWWWDCCCCC" "..OWDWDCCCCC"
        ".OWWWWDCCCCC" ".OWDWWDCCCCC" ".OWWWWDCCCCC" ".OWWDWDCCCCC" ".OWWWWDCCCCC";

    /// Tuft sprites, rows 0..5 x cols 0..11 of the left side; mirrored for
    /// the right except 'wild' (horns left, WILD_RIGHT mirrored right).
    bytes private constant TUFT_TUFTS =
        "............" "............" "....O......." "....ODO....." "....ODDO...." ".....D......";
    bytes private constant TUFT_HORNS =
        "..O........." "..OO........" "..ODO......." "..ODDO......" "...ODDO....." ".....D......";
    bytes private constant TUFT_TALL =
        ".....O......" "....ODO....." "....ODO....." "....ODDO...." "....ODDO...." ".....DD.....";
    bytes private constant TUFT_CREST =
        "............" "...........O" "........O.OD" "........ODOD" ".........DDD" "............";
    bytes private constant WILD_RIGHT =
        "............" "............" "............" "..OOO......." "...ODDO....." ".....D......";

    /// Chevron chest mask, half coords x 7..11, rows 17..23.
    bytes private constant CHEVRON = "....." "D...." ".D..D" "..DD." "D...." ".D..D" "..DD.";

    // Accessory sprites and burn embers: (x, y, symbol) triples, stamped in order.
    bytes private constant ACC_CHAIN =
        hex"06104b07114f08114b09124f0a124b0d124b0e124f0f114b10114f11104b0b124f0c124f0a134f0b134b0c134b0d134f0a144f0b14590c14590d144f0b154f0c154f";
    bytes private constant ACC_HEADBAND =
        hex"0406520506520606520706520806520906520a06520b06520c06520d06520e06520f06521006521106521206521306520605720705720805720905720a05720b05720c05720d05720e05720f0572100572110572030652020672020752010852020872010972";
    bytes private constant ACC_BOWTIE =
        hex"080f520f0f520810520910520a10520b10720c10720d10520e10520f10520811520f1152090f520e0f520911520e1152";
    bytes private constant ACC_MONOCLE =
        hex"0e074f0f074f10074f11074f0e0c4f0f0c4f100c4f110c4f0d084f0d094f0d0a4f0d0b4f12084f12094f120a4f120b4f11074b120c4b130d4f130e4b140f4f14104b";
    bytes private constant ACC_EARRING = hex"020d4b010e4b030e4b020f4b";
    bytes private constant ACC_PIPE = hex"0d0c540e0d540f0d54100c51110c51100d54110d54100e54110e54";
    /// Hollow 1px ring, 10 wide (x 7..16), rows 0..2, cream glints on the
    /// top edge. It first clears its hollow (row 1) and the air under it
    /// (rows 2..3, x 8..15) to background ('.'), so a full row of air always
    /// separates ring and crown (a crest is flattened to its base). G = halo
    /// gold: gold, or deep gold on the gold background.
    bytes private constant ACC_HALO =
        hex"09012e0a012e0b012e0c012e0d012e0e012e08022e09022e0a022e0b022e0c022e0d022e0e022e0f022e08032e09032e0a032e0b032e0c032e0d032e0e032e0f032e0900470a00480b00480c00470d00470e00470701470801470f01471001470902470a02470b02470c02470d02470e0247";
    bytes private constant EMBERS = hex"01025115014b130451020b4b160851001151170d4b04014b16144b01154b";

    /// Per species: B, D, W, L, E, N as 3-byte RGB.
    bytes private constant SPECIES_RGB =
        hex"B0841D9670196B500FCDA24AFBF3DCF4B728" hex"F4B728B0841D967019F9D57AFBF3DC967019"
        hex"9670196B500F4A370BB38C3AFBF3DCF4B728" hex"D8B56AA67C3B8A6430F4E9CFFBF3DCF4B728"
        hex"7B8F3E5C6B2E414D209CAD5EFBF3DCF4B728" hex"C25A3394422B6E2F1EDB8058FBF3DCF4B728"
        hex"7E6BA85D4E80443A61A08FC4FBF3DCF4B728" hex"7C87925A636D414951A0AAB3FBF3DCF4B728"
        hex"E8E4D8B9B4A58F8B7EFAF8F1F4B728F4B728" hex"3E4A6B2C35501E243856648AFBF3DCF4B728"
        hex"FBF3DCB0841DE4D9B8FFFDF6F4B728B0841D";
    bytes private constant BG_RGB = hex"3D362E5B6F7E6E7C63FBF3DC94644A1C1914F4B7281C1914";
    bytes private constant IRIS_RGB = hex"000000E8892BD5452B3A3129";

    // ---------------------------------------------------------------
    // Grid assembly (mirrors ashwing24.py `grid`, step for step)
    // ---------------------------------------------------------------

    /// Build the 24x24 symbol grid (576 bytes, row-major).
    function _grid(bytes32 seed) internal pure returns (bytes memory g, Traits memory t) {
        t = _traits(seed);
        bytes memory half = HALF;

        // Half-space: sleepy lids, symmetric chest patterns.
        if (t.eyes == 1) {
            half[8 * 12 + 7] = "D";
            half[8 * 12 + 8] = "D";
            for (uint256 x = 6; x < 10; x++) {
                half[9 * 12 + x] = "D";
            }
        }
        if (t.chest != 0) {
            bytes memory chev = CHEVRON;
            for (uint256 y = 17; y < 24; y++) {
                for (uint256 x = 7; x < 12; x++) {
                    uint256 i = y * 12 + x;
                    if (half[i] != "C") continue;
                    if (t.chest == 1) {
                        half[i] = y % 2 == 0 ? bytes1("D") : bytes1("B");
                    } else if (t.chest == 2) {
                        half[i] = chev[(y - 17) * 5 + (x - 7)] == "D" ? bytes1("D") : bytes1("B");
                    } else {
                        uint256 lim = 10 - (y - 17);
                        if (lim < 8) lim = 8;
                        half[i] = x >= lim ? bytes1("L") : bytes1("B");
                    }
                }
            }
        }

        // Mirror.
        g = new bytes(576);
        for (uint256 y = 0; y < 24; y++) {
            for (uint256 x = 0; x < 24; x++) {
                g[y * 24 + x] = half[y * 12 + (x < 12 ? x : 23 - x)];
            }
        }

        // Tufts.
        if (t.tufts == 5) {
            _stampRows(g, TUFT_HORNS, false);
            _stampRows(g, WILD_RIGHT, true);
        } else if (t.tufts != 0) {
            bytes memory s = t.tufts == 1 ? TUFT_TUFTS : t.tufts == 2 ? TUFT_HORNS : t.tufts == 3 ? TUFT_TALL : TUFT_CREST;
            _stampRows(g, s, false);
            _stampRows(g, s, true);
        }

        // Speckle chest: 2 seed bits per chest cell, spots on a staggered
        // lattice of even rows (none / shade / shade / wing-dark).
        if (t.chest == 0) {
            uint256 s = uint256(seed);
            for (uint256 y = 17; y < 24; y++) {
                for (uint256 x = 7; x < 17; x++) {
                    bytes1 c = "B";
                    if (y % 2 == 0 && (x + y / 2) % 2 == 0) {
                        uint256 v = (s >> (2 * ((y - 17) * 10 + (x - 7)))) & 3;
                        c = v == 0 ? bytes1("B") : v == 3 ? bytes1("W") : bytes1("D");
                    }
                    g[y * 24 + x] = c;
                }
            }
        }

        // Beak, centre columns 11 and 12.
        for (uint256 x = 11; x < 13; x++) {
            g[10 * 24 + x] = "N";
            g[11 * 24 + x] = "N";
            if (t.beak == 1 || t.beak == 2) g[12 * 24 + x] = "N";
            if (t.beak == 3) {
                g[11 * 24 + x] = "O";
                g[12 * 24 + x] = "N";
            }
        }
        if (t.beak == 2) g[13 * 24 + 11] = "N";

        // Pupils after the mirror: the same dx for both eyes.
        if (t.eyes != 2) {
            bool tall = t.pupil == 1 && t.eyes != 1;
            uint256 y0 = t.eyes == 1 ? 10 : 9;
            uint256 y1 = t.eyes == 1 ? 10 : (tall ? 11 : 10);
            for (uint256 e = 0; e < 2; e++) {
                uint256 px = uint256(int256(e == 0 ? int256(7) : int256(15)) + t.dx); // x0 + 1 + dx
                for (uint256 yy = y0; yy <= y1; yy++) {
                    g[yy * 24 + px] = "O";
                    g[yy * 24 + px + 1] = "O";
                }
            }
        }
        if (t.eyes == 2) {
            _closeEye(g, 6);
            _closeEye(g, 14);
        }
        if (t.eyes == 3) _closeEye(g, 14);

        // Void iris catchlight: one cream pixel on the pupil's top row, on
        // the glance side (left cell for a stare); open eyes only.
        if (t.iris == 3 && t.eyes != 2) {
            uint256 yy = t.eyes == 1 ? 10 : 9;
            for (uint256 e = 0; e < (t.eyes == 3 ? 1 : 2); e++) {
                int256 x0 = e == 0 ? int256(6) : int256(14);
                int256 cx = t.dx <= 0 ? x0 + 1 + t.dx : x0 + 2 + t.dx;
                g[yy * 24 + uint256(cx)] = "H";
            }
        }

        // Burn background embers (background cells only).
        if (t.background == 7) {
            bytes memory em = EMBERS;
            for (uint256 k = 0; k < em.length; k += 3) {
                uint256 i = uint256(uint8(em[k + 1])) * 24 + uint8(em[k]);
                if (g[i] == ".") g[i] = em[k + 2];
            }
        }

        // Accessory.
        if (t.accessory != 0) {
            bytes memory a = t.accessory == 1
                ? ACC_CHAIN
                : t.accessory == 2
                    ? ACC_HEADBAND
                    : t.accessory == 3
                        ? ACC_BOWTIE
                        : t.accessory == 4 ? ACC_MONOCLE : t.accessory == 5 ? ACC_EARRING : t.accessory == 6 ? ACC_PIPE : ACC_HALO;
            for (uint256 k = 0; k < a.length; k += 3) {
                g[uint256(uint8(a[k + 1])) * 24 + uint8(a[k])] = a[k + 2];
            }
        }
    }

    /// Stamp a 6x12 row sprite onto rows 0..5, left side or mirrored right.
    function _stampRows(bytes memory g, bytes memory s, bool mirror) internal pure {
        for (uint256 y = 0; y < 6; y++) {
            for (uint256 x = 0; x < 12; x++) {
                bytes1 c = s[y * 12 + x];
                if (c != ".") g[y * 24 + (mirror ? 23 - x : x)] = c;
            }
        }
    }

    /// Closed eye in the 4x4 box at (x0, 8): lid fill plus a smiling lash.
    function _closeEye(bytes memory g, uint256 x0) internal pure {
        for (uint256 y = 8; y < 12; y++) {
            for (uint256 x = x0; x < x0 + 4; x++) {
                bytes1 c = g[y * 24 + x];
                if (c == "E" || c == "O") g[y * 24 + x] = "D";
            }
        }
        g[8 * 24 + x0 + 1] = "L";
        g[8 * 24 + x0 + 2] = "L";
        g[9 * 24 + x0] = "O";
        g[9 * 24 + x0 + 3] = "O";
        g[10 * 24 + x0 + 1] = "O";
        g[10 * 24 + x0 + 2] = "O";
    }

    /// Symbol -> 0xRRGGBB, indexed by the symbol's ASCII code.
    function _palette(Traits memory t) internal pure returns (uint256[128] memory pal) {
        pal[uint8(bytes1("O"))] = 0x1C1914;
        pal[uint8(bytes1("K"))] = 0xF4B728;
        pal[uint8(bytes1("Y"))] = 0xB0841D;
        pal[uint8(bytes1("R"))] = 0xB8322A;
        pal[uint8(bytes1("r"))] = 0x7E1F1A;
        pal[uint8(bytes1("T"))] = 0x6B4428;
        pal[uint8(bytes1("Q"))] = 0xF07A2A;
        pal[uint8(bytes1("S"))] = 0xB9B2A6;
        pal[uint8(bytes1("H"))] = 0xFBF3DC;
        bytes memory sp = SPECIES_RGB;
        uint256 o = uint256(t.species) * 18;
        pal[uint8(bytes1("B"))] = _rgb(sp, o);
        pal[uint8(bytes1("D"))] = _rgb(sp, o + 3);
        pal[uint8(bytes1("W"))] = _rgb(sp, o + 6);
        pal[uint8(bytes1("L"))] = _rgb(sp, o + 9);
        pal[uint8(bytes1("E"))] = t.iris == 0 ? _rgb(sp, o + 12) : _rgb(IRIS_RGB, uint256(t.iris) * 3);
        pal[uint8(bytes1("N"))] = _rgb(sp, o + 15);
        pal[uint8(bytes1("."))] = _rgb(BG_RGB, uint256(t.background) * 3);
        // Halo gold: a gold ring would vanish on the gold background.
        pal[uint8(bytes1("G"))] = t.background == 6 ? 0xB0841D : 0xF4B728;
    }

    function _rgb(bytes memory b, uint256 o) internal pure returns (uint256) {
        return (uint256(uint8(b[o])) << 16) | (uint256(uint8(b[o + 1])) << 8) | uint8(b[o + 2]);
    }

    // ---------------------------------------------------------------
    // Rendering: one full-canvas background rect, then per row one rect
    // per run of equal colour (runs in the background colour skipped).
    // ---------------------------------------------------------------

    function svgOf(uint256 id) public view returns (string memory) {
        require(ownerOf[id] != address(0), "ASHW: unminted");
        return _svg(seedOf[id]);
    }

    function _svg(bytes32 seed) internal pure returns (string memory) {
        (bytes memory g, Traits memory t) = _grid(seed);
        uint256[128] memory pal = _palette(t);
        uint256 bg = pal[uint8(bytes1("."))];
        return string(
            abi.encodePacked(
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" shape-rendering="crispEdges"><rect width="24" height="24" fill="#',
                _hex6(bg),
                '"/>',
                _rects(g, pal, bg),
                "</svg>"
            )
        );
    }

    function _hex6(uint256 c) internal pure returns (bytes memory h) {
        h = new bytes(6);
        bytes16 digits = "0123456789ABCDEF";
        for (uint256 i = 0; i < 6; i++) {
            h[i] = digits[(c >> (20 - 4 * i)) & 0xF];
        }
    }

    /// Row run-length rects, written straight into one buffer. A rect is
    /// at most 58 bytes; 24x24 cells bound the count at 576.
    function _rects(bytes memory g, uint256[128] memory pal, uint256 bg) internal pure returns (bytes memory out) {
        out = new bytes(576 * 58 + 64);
        assembly {
            // Write n (0..24) as decimal; returns the advanced pointer.
            function num(p, n) -> q {
                switch lt(n, 10)
                case 1 {
                    mstore8(p, add(48, n))
                    q := add(p, 1)
                }
                default {
                    mstore8(p, add(48, div(n, 10)))
                    mstore8(add(p, 1), add(48, mod(n, 10)))
                    q := add(p, 2)
                }
            }
            let hexd := "0123456789ABCDEF"
            let start := add(out, 32)
            let p := start
            let cells := add(g, 32)
            for { let y := 0 } lt(y, 24) { y := add(y, 1) } {
                let row := add(cells, mul(y, 24))
                let x := 0
                for {} lt(x, 24) {} {
                    let c := mload(add(pal, shl(5, byte(0, mload(add(row, x))))))
                    let x1 := add(x, 1)
                    for {} lt(x1, 24) { x1 := add(x1, 1) } {
                        if iszero(eq(c, mload(add(pal, shl(5, byte(0, mload(add(row, x1)))))))) { break }
                    }
                    if iszero(eq(c, bg)) {
                        mstore(p, '<rect x="')
                        p := num(add(p, 9), x)
                        mstore(p, '" y="')
                        p := num(add(p, 5), y)
                        mstore(p, '" width="')
                        p := num(add(p, 9), sub(x1, x))
                        mstore(p, '" height="1" fill="#')
                        p := add(p, 20)
                        for { let i := 0 } lt(i, 6) { i := add(i, 1) } {
                            mstore8(add(p, i), byte(and(shr(sub(20, shl(2, i)), c), 0xF), hexd))
                        }
                        mstore(add(p, 6), '"/>')
                        p := add(p, 9)
                    }
                    x := x1
                }
            }
            mstore(out, sub(p, start))
        }
    }

    // ---------------------------------------------------------------
    // Metadata
    // ---------------------------------------------------------------

    function tokenURI(uint256 id) external view returns (string memory) {
        require(ownerOf[id] != address(0), "ASHW: unminted");
        bytes32 seed = seedOf[id];
        string memory json = string.concat(
            '{"name":"Ashwing #',
            _toString(id),
            '","description":"A fully on-chain owl, minted with destroyed money: every wei of SOVA gas traces back to burned ZEC.","attributes":',
            _attributes(_traits(seed)),
            ',"image":"data:image/svg+xml;base64,',
            _base64(bytes(_svg(seed))),
            '"}'
        );
        return string.concat("data:application/json;base64,", _base64(bytes(json)));
    }

    function _attributes(Traits memory t) internal pure returns (string memory) {
        string[11] memory species =
            ["classic", "bright", "bronze", "barn", "moss", "ember", "dusk", "slate", "snowy", "midnight", "spectral"];
        string[8] memory backgrounds = ["ash", "slate", "sage", "cream", "clay", "night", "gold", "burn"];
        string[6] memory tufts = ["none", "tufts", "horns", "tall", "crest", "wild"];
        string[4] memory irises = ["natural", "amber", "ember", "void"];
        string[4] memory beaks = ["small", "long", "hooked", "hoot"];
        string[4] memory chests = ["speckle", "bars", "chevrons", "bib"];
        string[8] memory accessories =
            ["none", "gold chain", "headband", "bow tie", "monocle", "earring", "pipe", "halo"];
        string memory eyes = t.eyes == 2
            ? "zen"
            : t.eyes == 1
                ? "sleepy"
                : t.eyes == 3
                    ? "wink"
                    : t.dx != 0 ? (t.dx < 0 ? "glance left" : "glance right") : (t.pupil == 1 ? "wide stare" : "stare");
        return string.concat(
            string.concat(
                '[{"trait_type":"species","value":"',
                species[t.species],
                '"},{"trait_type":"background","value":"',
                backgrounds[t.background],
                '"},{"trait_type":"tufts","value":"',
                tufts[t.tufts],
                '"},{"trait_type":"eyes","value":"',
                eyes
            ),
            string.concat(
                '"},{"trait_type":"iris","value":"',
                irises[t.iris],
                '"},{"trait_type":"beak","value":"',
                beaks[t.beak],
                '"},{"trait_type":"chest","value":"',
                chests[t.chest],
                '"},{"trait_type":"accessory","value":"',
                accessories[t.accessory],
                '"}]'
            )
        );
    }

    function _toString(uint256 value) internal pure returns (string memory) {
        if (value == 0) return "0";
        uint256 temp = value;
        uint256 digits;
        while (temp != 0) {
            digits++;
            temp /= 10;
        }
        bytes memory buffer = new bytes(digits);
        while (value != 0) {
            digits--;
            buffer[digits] = bytes1(uint8(48 + (value % 10)));
            value /= 10;
        }
        return string(buffer);
    }

    function _base64(bytes memory data) internal pure returns (string memory) {
        if (data.length == 0) return "";
        string memory table = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        string memory result = new string(4 * ((data.length + 2) / 3));
        assembly {
            let tablePtr := add(table, 1)
            let resultPtr := add(result, 32)
            for { let dataPtr := data } lt(dataPtr, add(data, mload(data))) {} {
                dataPtr := add(dataPtr, 3)
                let input := mload(dataPtr)
                mstore8(resultPtr, mload(add(tablePtr, and(shr(18, input), 0x3F))))
                resultPtr := add(resultPtr, 1)
                mstore8(resultPtr, mload(add(tablePtr, and(shr(12, input), 0x3F))))
                resultPtr := add(resultPtr, 1)
                mstore8(resultPtr, mload(add(tablePtr, and(shr(6, input), 0x3F))))
                resultPtr := add(resultPtr, 1)
                mstore8(resultPtr, mload(add(tablePtr, and(input, 0x3F))))
                resultPtr := add(resultPtr, 1)
            }
            switch mod(mload(data), 3)
            case 1 {
                mstore8(sub(resultPtr, 1), 0x3d)
                mstore8(sub(resultPtr, 2), 0x3d)
            }
            case 2 { mstore8(sub(resultPtr, 1), 0x3d) }
        }
        return result;
    }
}

interface IERC721Receiver {
    function onERC721Received(address operator, address from, uint256 id, bytes calldata data)
        external
        returns (bytes4);
}
