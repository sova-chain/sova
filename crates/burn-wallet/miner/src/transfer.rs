//! `sova-miner transfer`: send SOVA from the keystore key's own EVM address
//! without ever exporting the key.
//!
//! The SOVA a miner earns lands on the Ethereum address of its keystore key
//! (see [`crate::evm_address`]). `export-evm-key` hands that key to a wallet;
//! this command instead signs one plain value transfer in-process and
//! broadcasts it, so the key never leaves the keystore file: it is not
//! printed, logged or written anywhere.
//!
//! The transaction is an EIP-1559 (type 2) transfer: 21,000 gas, empty
//! calldata, empty access list, on the chain id the Sova node reports. The
//! encoding is hand-rolled RLP (a transfer has eight scalar fields; no
//! RLP/alloy dependency is worth that), hashed with the Keccak-256 this
//! crate already uses for addresses, and signed with the same `secp256k1`
//! the keystore uses (its `recovery` feature: a recoverable signature is
//! what gives EIP-1559's `y_parity`). libsecp256k1 signs with RFC 6979
//! nonces and always emits low-S signatures (EIP-2), so the bytes match
//! any other RFC 6979 signer's -- the unit tests pin them to `cast mktx`.
//!
//! Mainnet safety: only chain ids in [`KNOWN_CHAINS`] (the Sova testnet and
//! the local dev chains) are accepted unless `--any-chain` is given.

use std::io::BufRead;
use std::path::Path;
use std::time::{Duration, Instant};

use burn_wallet::Keypair;
use burn_wallet::rpc::{RpcClient, RpcError};
use secp256k1::{Message, Secp256k1, SecretKey};
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};

use crate::evm_address::derive_evm_address;

/// Wei per SOVA (SOVA has 18 decimals, like ETH).
pub(crate) const WEI_PER_SOVA: u128 = 1_000_000_000_000_000_000;

/// Gas of a plain value transfer to an account.
pub(crate) const TRANSFER_GAS: u64 = 21_000;

/// Chain ids `transfer` sends on without `--any-chain`.
pub(crate) const KNOWN_CHAINS: &[(u64, &str)] = &[
    (82_330, "Sova testnet"),
    (31_337, "local dev chain (anvil)"),
    (1_337, "local dev chain (reth --dev / regtest)"),
];

/// Priority fee used when the node has no `eth_maxPriorityFeePerGas`.
const FALLBACK_PRIORITY_FEE: u128 = 1_000_000_000;

/// How often to poll for the receipt.
const RECEIPT_POLL: Duration = Duration::from_secs(2);

/// Per-request timeout against the Sova node.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Parses a decimal SOVA amount (`"1"`, `"0.5"`, `"12.000000000000000001"`)
/// into wei. Plain digits with an optional fractional part of 1 to 18
/// digits; no sign, exponent, separators or whitespace. Zero is refused.
///
/// # Errors
///
/// A human-readable reason when the amount is malformed, has more than 18
/// decimals, is zero, or overflows.
pub(crate) fn parse_sova_amount(s: &str) -> Result<u128, String> {
    let bad = || format!("invalid SOVA amount {s:?}: expected a decimal like 1 or 0.25");
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (s, None),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let frac = frac.unwrap_or("");
    if s.contains('.') && frac.is_empty() {
        return Err(bad());
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    if frac.len() > 18 {
        return Err(format!(
            "invalid SOVA amount {s:?}: at most 18 decimal places (1 wei = 0.000000000000000001 SOVA)"
        ));
    }
    let overflow = || format!("SOVA amount {s:?} is too large");
    let whole: u128 = whole.parse().map_err(|_| overflow())?;
    let frac_wei: u128 = if frac.is_empty() {
        0
    } else {
        let padded = format!("{frac:0<18}");
        padded.parse().map_err(|_| bad())?
    };
    let wei = whole
        .checked_mul(WEI_PER_SOVA)
        .and_then(|w| w.checked_add(frac_wei))
        .ok_or_else(overflow)?;
    if wei == 0 {
        return Err(format!("SOVA amount {s:?} is zero"));
    }
    Ok(wei)
}

/// Formats wei as a decimal SOVA amount, trailing zeros trimmed.
#[must_use]
pub(crate) fn format_sova(wei: u128) -> String {
    let whole = wei / WEI_PER_SOVA;
    let frac = wei % WEI_PER_SOVA;
    if frac == 0 {
        return whole.to_string();
    }
    let frac = format!("{frac:018}");
    format!("{whole}.{}", frac.trim_end_matches('0'))
}

/// Refuses chain ids outside [`KNOWN_CHAINS`] unless `any_chain`.
///
/// # Errors
///
/// Why the chain was refused.
pub(crate) fn check_chain(chain_id: u64, any_chain: bool) -> Result<&'static str, String> {
    if let Some((_, name)) = KNOWN_CHAINS.iter().find(|(id, _)| *id == chain_id) {
        return Ok(name);
    }
    if any_chain {
        return Ok("unrecognised chain (--any-chain)");
    }
    let known: Vec<String> = KNOWN_CHAINS
        .iter()
        .map(|(id, name)| format!("{id} ({name})"))
        .collect();
    Err(format!(
        "refusing to send on chain id {chain_id}: transfer only sends on {} unless --any-chain is given",
        known.join(", ")
    ))
}

// ---- RLP ----------------------------------------------------------------

fn rlp_length_prefix(len: usize, short_base: u8, out: &mut Vec<u8>) {
    if len <= 55 {
        // `len` <= 55 fits in a byte.
        out.push(short_base + len as u8);
    } else {
        let len_bytes = (len as u64).to_be_bytes();
        let skip = len_bytes.iter().take_while(|b| **b == 0).count();
        let len_bytes = &len_bytes[skip..];
        out.push(short_base + 55 + len_bytes.len() as u8);
        out.extend_from_slice(len_bytes);
    }
}

fn rlp_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        out.push(bytes[0]);
    } else {
        rlp_length_prefix(bytes.len(), 0x80, out);
        out.extend_from_slice(bytes);
    }
}

/// An RLP scalar: big-endian, no leading zeros (zero is the empty string).
fn rlp_uint(bytes_be: &[u8], out: &mut Vec<u8>) {
    let skip = bytes_be.iter().take_while(|b| **b == 0).count();
    rlp_bytes(&bytes_be[skip..], out);
}

fn rlp_list(payload: &[u8], out: &mut Vec<u8>) {
    rlp_length_prefix(payload.len(), 0xc0, out);
    out.extend_from_slice(payload);
}

// ---- The transaction ------------------------------------------------------

/// An unsigned EIP-1559 plain value transfer (no calldata, no access list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Transfer {
    pub(crate) chain_id: u64,
    pub(crate) nonce: u64,
    pub(crate) max_priority_fee_per_gas: u128,
    pub(crate) max_fee_per_gas: u128,
    pub(crate) gas_limit: u64,
    pub(crate) to: [u8; 20],
    pub(crate) value: u128,
}

/// A signed transfer, ready for `eth_sendRawTransaction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignedTransfer {
    /// `0x02 || rlp([...fields, y_parity, r, s])`.
    pub(crate) raw: Vec<u8>,
    /// The transaction hash: `keccak256(raw)`.
    pub(crate) hash: [u8; 32],
}

impl Transfer {
    /// The RLP payload of the eight unsigned fields plus the empty
    /// calldata and access list.
    fn fields_payload(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(96);
        rlp_uint(&self.chain_id.to_be_bytes(), &mut p);
        rlp_uint(&self.nonce.to_be_bytes(), &mut p);
        rlp_uint(&self.max_priority_fee_per_gas.to_be_bytes(), &mut p);
        rlp_uint(&self.max_fee_per_gas.to_be_bytes(), &mut p);
        rlp_uint(&self.gas_limit.to_be_bytes(), &mut p);
        rlp_bytes(&self.to, &mut p);
        rlp_uint(&self.value.to_be_bytes(), &mut p);
        rlp_bytes(&[], &mut p); // data
        rlp_list(&[], &mut p); // access list
        p
    }

    /// The EIP-1559 signing hash: `keccak256(0x02 || rlp([fields]))`.
    #[must_use]
    pub(crate) fn signing_hash(&self) -> [u8; 32] {
        let mut encoded = vec![0x02];
        rlp_list(&self.fields_payload(), &mut encoded);
        Keccak256::digest(&encoded).into()
    }

    /// Signs with `secret` (RFC 6979, low-S).
    #[must_use]
    pub(crate) fn sign(&self, secret: &SecretKey) -> SignedTransfer {
        let secp = Secp256k1::signing_only();
        let msg = Message::from_digest(self.signing_hash());
        let (recid, compact) = secp
            .sign_ecdsa_recoverable(&msg, secret)
            .serialize_compact();
        let mut payload = self.fields_payload();
        // Recovery ids from libsecp256k1 are 0..=3; 2 and 3 (r >= n) never
        // occur in practice and EIP-1559 only defines 0/1.
        rlp_uint(&[(recid.to_i32() & 1) as u8], &mut payload);
        rlp_uint(&compact[..32], &mut payload);
        rlp_uint(&compact[32..], &mut payload);
        let mut raw = vec![0x02];
        rlp_list(&payload, &mut raw);
        let hash = Keccak256::digest(&raw).into();
        SignedTransfer { raw, hash }
    }
}

// ---- RPC helpers ------------------------------------------------------------

fn quantity(method: &str, v: &Value) -> Result<u128, String> {
    let s = v
        .as_str()
        .ok_or_else(|| format!("{method}: expected a hex quantity, got {v}"))?;
    let digits = s
        .strip_prefix("0x")
        .ok_or_else(|| format!("{method}: expected a 0x quantity, got {s:?}"))?;
    if digits.is_empty() {
        return Err(format!("{method}: empty quantity"));
    }
    u128::from_str_radix(digits, 16).map_err(|e| format!("{method}: bad quantity {s:?}: {e}"))
}

fn call_quantity(rpc: &RpcClient, method: &str, params: Value) -> Result<u128, String> {
    let v = rpc.call_method(method, params).map_err(|e| e.to_string())?;
    quantity(method, &v)
}

/// The next block's base fee: the last entry of `eth_feeHistory`'s
/// `baseFeePerGas` (which includes the pending block), else the latest
/// block's `baseFeePerGas`.
fn next_base_fee(rpc: &RpcClient) -> Result<u128, String> {
    if let Ok(hist) = rpc.call_method("eth_feeHistory", json!(["0x1", "latest", []]))
        && let Some(last) = hist
            .get("baseFeePerGas")
            .and_then(Value::as_array)
            .and_then(|a| a.last())
    {
        return quantity("eth_feeHistory", last);
    }
    let block = rpc
        .call_method("eth_getBlockByNumber", json!(["latest", false]))
        .map_err(|e| e.to_string())?;
    let base = block
        .get("baseFeePerGas")
        .ok_or("the Sova node reports no base fee (not an EIP-1559 chain?)")?;
    quantity("eth_getBlockByNumber", base)
}

/// Fees for the transfer: priority fee from `eth_maxPriorityFeePerGas`
/// (1 gwei if unsupported), max fee `2 * base + tip` (headroom for six
/// full blocks of base-fee growth). Refused if above `cap`.
fn fees(rpc: &RpcClient, cap: u128) -> Result<(u128, u128, u128), String> {
    let base = next_base_fee(rpc)?;
    let tip =
        call_quantity(rpc, "eth_maxPriorityFeePerGas", json!([])).unwrap_or(FALLBACK_PRIORITY_FEE);
    let max_fee = base
        .checked_mul(2)
        .and_then(|b| b.checked_add(tip))
        .ok_or("fee overflow")?;
    if max_fee > cap {
        return Err(format!(
            "max fee per gas {max_fee} wei (base {base} + tip {tip}) is above --max-fee-per-gas {cap}; \
             refusing. Raise the cap if the chain really is that expensive"
        ));
    }
    Ok((base, tip, max_fee))
}

// ---- The command ------------------------------------------------------------

/// Arguments of `sova-miner transfer`.
#[derive(Debug)]
pub(crate) struct TransferArgs {
    pub(crate) to: [u8; 20],
    pub(crate) amount_wei: u128,
    pub(crate) sova_rpc: String,
    pub(crate) yes: bool,
    pub(crate) any_chain: bool,
    pub(crate) max_fee_per_gas: u128,
    pub(crate) wait: Duration,
}

fn addr(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

/// Asks on stderr/stdin; only `y`/`yes` confirms (EOF does not).
fn confirm_interactively() -> bool {
    eprint!("send this transaction? type 'yes' to confirm: ");
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
    }
}

/// `sova-miner transfer`. Loads the keystore like every other subcommand,
/// signs in memory, broadcasts, and waits for the receipt.
///
/// # Errors
///
/// Any refusal (unknown chain, insufficient balance, fee above the cap, not
/// confirmed), RPC failure, a reverted/failed receipt, or a timeout.
pub(crate) fn run(ks_path: &Path, args: TransferArgs) -> Result<(), String> {
    let keypair = Keypair::load_from_file(ks_path).map_err(|e| {
        format!(
            "{e} (run `sova-miner init` first -- expected a keystore at {})",
            ks_path.display()
        )
    })?;
    let from = derive_evm_address(&keypair);
    if args.to == [0u8; 20] {
        return Err("refusing to send to the zero address".into());
    }

    let rpc = RpcClient::with_timeout(args.sova_rpc.clone(), RPC_TIMEOUT);
    let chain_id = u64::try_from(call_quantity(&rpc, "eth_chainId", json!([]))?)
        .map_err(|_| "eth_chainId does not fit in 64 bits".to_string())?;
    let chain_name = check_chain(chain_id, args.any_chain)?;

    let nonce = u64::try_from(call_quantity(
        &rpc,
        "eth_getTransactionCount",
        json!([addr(&from), "pending"]),
    )?)
    .map_err(|_| "nonce does not fit in 64 bits".to_string())?;
    let balance = call_quantity(&rpc, "eth_getBalance", json!([addr(&from), "latest"]))?;
    let (base, tip, max_fee) = fees(&rpc, args.max_fee_per_gas)?;
    let max_total_fee = max_fee * u128::from(TRANSFER_GAS);

    let tx = Transfer {
        chain_id,
        nonce,
        max_priority_fee_per_gas: tip,
        max_fee_per_gas: max_fee,
        gas_limit: TRANSFER_GAS,
        to: args.to,
        value: args.amount_wei,
    };

    println!("from:        {} (this keystore key)", addr(&from));
    println!("to:          {}", addr(&args.to));
    println!(
        "amount:      {} SOVA ({} wei)",
        format_sova(args.amount_wei),
        args.amount_wei
    );
    println!("chain id:    {chain_id} ({chain_name})");
    println!("rpc:         {}", args.sova_rpc);
    println!("nonce:       {nonce}");
    println!(
        "fee:         at most {} SOVA ({TRANSFER_GAS} gas x {max_fee} wei; base {base}, tip {tip})",
        format_sova(max_total_fee)
    );
    println!("balance:     {} SOVA", format_sova(balance));

    let needed = args
        .amount_wei
        .checked_add(max_total_fee)
        .ok_or("amount + fee overflows")?;
    if balance < needed {
        return Err(format!(
            "insufficient balance: {} SOVA available, {} SOVA needed (amount + max fee)",
            format_sova(balance),
            format_sova(needed)
        ));
    }

    if !args.yes && !confirm_interactively() {
        return Err("not confirmed; nothing sent (pass --yes to skip the prompt)".into());
    }

    let signed = tx.sign(&keypair.secret_key());
    let local_hash = format!("0x{}", hex::encode(signed.hash));
    let sent = rpc
        .call_method(
            "eth_sendRawTransaction",
            json!([format!("0x{}", hex::encode(&signed.raw))]),
        )
        .map_err(|e: RpcError| format!("eth_sendRawTransaction failed: {e}"))?;
    let hash = sent.as_str().unwrap_or(&local_hash).to_string();
    if !hash.eq_ignore_ascii_case(&local_hash) {
        eprintln!("note: node returned tx hash {hash}, locally computed {local_hash}");
    }
    println!("tx hash:     {hash}");
    println!(
        "waiting for the receipt (up to {}s; Sova makes a block per Zcash block)...",
        args.wait.as_secs()
    );

    let deadline = Instant::now() + args.wait;
    loop {
        match rpc.call_method("eth_getTransactionReceipt", json!([hash])) {
            Ok(r) if !r.is_null() => return report_receipt(&r),
            Ok(_) => {}
            // A transient failure is not the end: the tx is already out.
            Err(e) => eprintln!("receipt poll failed (will retry): {e}"),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no receipt for {hash} after {}s; it may still be mined -- check it on the explorer \
                 before sending again (a resend with the same nonce replaces, a new one doubles)",
                args.wait.as_secs()
            ));
        }
        std::thread::sleep(RECEIPT_POLL);
    }
}

fn report_receipt(r: &Value) -> Result<(), String> {
    let status = r.get("status").and_then(Value::as_str).unwrap_or("?");
    let block = r
        .get("blockNumber")
        .map(|b| quantity("blockNumber", b).map_or_else(|_| b.to_string(), |n| n.to_string()))
        .unwrap_or_default();
    let fee = match (r.get("gasUsed"), r.get("effectiveGasPrice")) {
        (Some(g), Some(p)) => match (quantity("gasUsed", g), quantity("effectiveGasPrice", p)) {
            (Ok(g), Ok(p)) => Some(g.saturating_mul(p)),
            _ => None,
        },
        _ => None,
    };
    println!("block:       {block}");
    if let Some(fee) = fee {
        println!("fee paid:    {} SOVA", format_sova(fee));
    }
    if status == "0x1" {
        println!("status:      success");
        Ok(())
    } else {
        println!("status:      FAILED ({status})");
        Err(format!("transaction failed on chain (status {status})"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};

    use super::*;

    fn key(last_byte: u8) -> Keypair {
        let mut secret = [0u8; 32];
        secret[31] = last_byte;
        Keypair::from_secret_bytes(secret).unwrap()
    }

    const DEAD: [u8; 20] = {
        let mut a = [0u8; 20];
        a[18] = 0xde;
        a[19] = 0xad;
        a
    };

    /// Minimal RLP decoder for a flat list (enough to take a signed
    /// transfer apart in tests).
    fn rlp_decode_list(buf: &[u8]) -> Vec<Vec<u8>> {
        fn item(buf: &[u8]) -> (usize, usize, bool) {
            // (header len, payload len, is_list)
            let b = buf[0];
            match b {
                0x00..=0x7f => (0, 1, false),
                0x80..=0xb7 => (1, (b - 0x80) as usize, false),
                0xb8..=0xbf => {
                    let n = (b - 0xb7) as usize;
                    let len = buf[1..=n].iter().fold(0usize, |a, x| a << 8 | *x as usize);
                    (1 + n, len, false)
                }
                0xc0..=0xf7 => (1, (b - 0xc0) as usize, true),
                _ => {
                    let n = (b - 0xf7) as usize;
                    let len = buf[1..=n].iter().fold(0usize, |a, x| a << 8 | *x as usize);
                    (1 + n, len, true)
                }
            }
        }
        let (h, len, is_list) = item(buf);
        assert!(is_list);
        assert_eq!(h + len, buf.len());
        let mut rest = &buf[h..];
        let mut out = Vec::new();
        while !rest.is_empty() {
            let (h, len, _) = item(rest);
            out.push(rest[h..h + len].to_vec());
            rest = &rest[h + len..];
        }
        out
    }

    /// Recovers the sender's EVM address from a signed transfer's bytes.
    fn recover_sender(tx: &Transfer, signed: &SignedTransfer) -> [u8; 20] {
        assert_eq!(signed.raw[0], 0x02);
        let fields = rlp_decode_list(&signed.raw[1..]);
        assert_eq!(fields.len(), 12);
        let y = fields[9].first().copied().unwrap_or(0);
        let mut compact = [0u8; 64];
        compact[32 - fields[10].len()..32].copy_from_slice(&fields[10]);
        compact[64 - fields[11].len()..].copy_from_slice(&fields[11]);
        let sig =
            RecoverableSignature::from_compact(&compact, RecoveryId::from_i32(y.into()).unwrap())
                .unwrap();
        let pk = Secp256k1::verification_only()
            .recover_ecdsa(&Message::from_digest(tx.signing_hash()), &sig)
            .unwrap();
        let hash = Keccak256::digest(&pk.serialize_uncompressed()[1..]);
        hash[12..].try_into().unwrap()
    }

    /// Byte-exact against Foundry: `cast mktx --private-key 0x..01
    /// --chain 82330 --nonce 7 --gas-limit 21000 --gas-price 1000000014
    /// --priority-gas-price 1000000000 --value 1500000000000000000
    /// 0x000000000000000000000000000000000000dEaD` (and a second, with
    /// zero-valued fields, to cover the empty-scalar encoding).
    #[test]
    fn signed_transfer_matches_cast_vectors() {
        let kp = key(1);
        let tx = Transfer {
            chain_id: 82_330,
            nonce: 7,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 1_000_000_014,
            gas_limit: 21_000,
            to: DEAD,
            value: 1_500_000_000_000_000_000,
        };
        let signed = tx.sign(&kp.secret_key());
        assert_eq!(
            hex::encode(&signed.raw),
            "02f8758301419a07843b9aca00843b9aca0e82520894000000000000000000000000000000000000dead\
             8814d1120d7b16000080c080a07b4076cfe958bcaaea8954202ffe4fdcbba6413c4b9c50582ef1f2d71d\
             5de303a07784f2d3147bb17ec7e164ca39ed79fd2a97bc55679d8560491a8f64ef4e01c8"
        );
        assert_eq!(
            signed.hash,
            <[u8; 32]>::from(Keccak256::digest(&signed.raw))
        );
        assert_eq!(recover_sender(&tx, &signed), derive_evm_address(&kp));

        let tx0 = Transfer {
            nonce: 0,
            max_priority_fee_per_gas: 0,
            max_fee_per_gas: 14,
            value: 1,
            ..tx
        };
        assert_eq!(
            hex::encode(tx0.sign(&kp.secret_key()).raw),
            "02f8658301419a80800e82520894000000000000000000000000000000000000dead0180c080a0d19741\
             5e28f1258ea69b48e42bb255a81081a6269276aefc87059d2410196c35a07aaf137d75a795b2554cd0d8\
             0506d294c7a318b3703443d50a9aec392f3667b5"
        );
    }

    /// A keystore written to disk and loaded back signs transfers that
    /// recover to that keystore's own EVM address (the one SOVA is minted
    /// to), across many keys and so both y-parities.
    #[test]
    fn signed_transfer_recovers_the_keystore_address() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let mut parities = [false; 2];
        for i in 0..16u64 {
            Keypair::generate().save_to_file(&path).unwrap();
            let kp = Keypair::load_from_file(&path).unwrap();
            let tx = Transfer {
                chain_id: 82_330,
                nonce: i,
                max_priority_fee_per_gas: 1,
                max_fee_per_gas: 15,
                gas_limit: TRANSFER_GAS,
                to: [0x42; 20],
                value: 47_000_000 * WEI_PER_SOVA + u128::from(i),
            };
            let signed = tx.sign(&kp.secret_key());
            assert_eq!(recover_sender(&tx, &signed), derive_evm_address(&kp));
            let fields = rlp_decode_list(&signed.raw[1..]);
            parities[usize::from(!fields[9].is_empty())] = true;
        }
        assert_eq!(parities, [true, true], "both y-parities exercised");
    }

    #[test]
    fn amount_parsing() {
        assert_eq!(parse_sova_amount("1").unwrap(), WEI_PER_SOVA);
        assert_eq!(parse_sova_amount("0.5").unwrap(), WEI_PER_SOVA / 2);
        assert_eq!(parse_sova_amount("000.25").unwrap(), WEI_PER_SOVA / 4);
        assert_eq!(parse_sova_amount("0.000000000000000001").unwrap(), 1);
        assert_eq!(
            parse_sova_amount("47000000.123456789012345678").unwrap(),
            47_000_000_123_456_789_012_345_678
        );
        assert_eq!(
            parse_sova_amount("1.10").unwrap(),
            1_100_000_000_000_000_000
        );

        let too_precise = parse_sova_amount("0.0000000000000000001").unwrap_err();
        assert!(too_precise.contains("18 decimal"), "{too_precise}");
        for bad in [
            "", "-1", "+1", "-0.5", "1e18", "0x10", "1,000", " 1", "1 ", ".5", "1.", "1..2",
            "1.2.3", "abc", "1.a", "SOVA", "∞", "0", "0.0", "0.000",
        ] {
            assert!(parse_sova_amount(bad).is_err(), "{bad:?} should be refused");
        }
        // u128 overflow.
        assert!(parse_sova_amount("340282366920938463464").is_err());
    }

    #[test]
    fn amount_formatting_roundtrips() {
        for s in [
            "1",
            "0.5",
            "0.000000000000000001",
            "47000000.123456789012345678",
        ] {
            assert_eq!(format_sova(parse_sova_amount(s).unwrap()), s);
        }
        assert_eq!(format_sova(0), "0");
    }

    #[test]
    fn chain_id_guard() {
        assert!(check_chain(82_330, false).is_ok());
        assert!(check_chain(31_337, false).is_ok());
        assert!(check_chain(1_337, false).is_ok());
        // Sova mainnet's reserved id, Ethereum mainnet, the old Sova ids.
        for id in [8_233, 1, 100_021, 120_893] {
            let err = check_chain(id, false).unwrap_err();
            assert!(err.contains("--any-chain"), "{err}");
            assert!(check_chain(id, true).is_ok());
        }
    }

    #[test]
    fn rlp_long_forms() {
        let mut out = Vec::new();
        rlp_bytes(&[0xaa; 56], &mut out);
        assert_eq!(&out[..2], &[0xb8, 56]);
        let mut out = Vec::new();
        rlp_list(&[0; 300], &mut out);
        assert_eq!(&out[..3], &[0xf9, 0x01, 0x2c]);
        let mut out = Vec::new();
        rlp_uint(&0u64.to_be_bytes(), &mut out);
        assert_eq!(out, [0x80]);
    }
}
