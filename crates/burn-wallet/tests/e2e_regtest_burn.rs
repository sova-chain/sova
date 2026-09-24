//! Empirical, end-to-end proof of the SIP-1 burn-transaction path against a
//! real Zebra regtest node (`box/regtest`).
//!
//! This builds a real, signed Zcash v5 transparent-only transaction with
//! `burn_wallet::tx::build_burn_transaction`, submits it to a live node via
//! `sendrawtransaction` -- exercising Zebra's real transaction-standardness
//! policy against our OP_RETURN + zero-hash-P2PKH outputs, which is the
//! whole point of doing this on a real node rather than only unit-testing
//! the builder -- mines it, fetches it back, and asserts that the on-chain
//! outputs round-trip through `consensus::sip1::extract_burn` with the
//! values we built.
//!
//! Ignored by default (it needs Docker and a live node). Run it via
//! `box/regtest/e2e-burn.sh`, or manually:
//!
//! ```text
//! cd box/regtest && docker compose up -d   # wait for it to report healthy
//! cd ../../crates/burn-wallet
//! cargo test --test e2e_regtest_burn -- --ignored --nocapture
//! ```
// Integration test: an unexpected `Err`/`None` from the harness or the
// builder is exactly the failure this test exists to catch, and `.expect()`
// with a message naming the failing step is more useful here than a custom
// `Result`-returning test harness would be.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use burn_wallet::rpc::RpcClient;
use burn_wallet::tx::{BurnTxRequest, Utxo, build_burn_transaction};
use burn_wallet::utxo::{encode_rpc_hash, find_spendable_coinbase};
use burn_wallet::{Keypair, Network};
use consensus::sip1::{TxOutRef, extract_burn};

const RPC_URL: &str = "http://127.0.0.1:18232";
const BURN_VALUE_ZAT: u64 = 100_000;
// ZIP-317's conventional fee is `marginal_fee (5000 zat) * max(grace_actions
// (2), logical_actions)`. Our tx has 1 transparent input and 3 transparent
// outputs (SIP-1 payload + eater + change), i.e. 3 logical actions, so the
// conventional fee is 5000 * 3 = 15_000 zat. Empirically, a 1_000 zat flat
// fee got rejected by Zebra's mempool with "failed to verify ZIP-317
// transaction rules ... Unpaid actions is higher than the limit" (RPC error
// -25) -- this is a ZIP-317 fee-sufficiency rejection, unrelated to the
// OP_RETURN/eater output *shapes* themselves (see the module docs). This
// fee comfortably clears the conventional-fee floor with margin.
const FEE_ZAT: u64 = 25_000;
const EVM_ADDRESS: [u8; 20] = [0xAB; 20];
const SIGNAL_BITS: u32 = 0x0000_0001;
/// Coinbase needs 100 confirmations to mature; mining 101 blocks total puts
/// the height-1 coinbase at exactly 100 confirmations (matured, spendable).
const BLOCKS_TO_MINE: u32 = 101;

#[test]
#[ignore = "requires the box/regtest docker harness; see box/regtest/e2e-burn.sh"]
fn e2e_regtest_burn() {
    let rpc = RpcClient::new(RPC_URL);

    let starting_height = rpc
        .get_block_count()
        .expect("zebrad RPC unreachable -- is box/regtest up? (see box/regtest/e2e-burn.sh)");

    // A fresh keypair, funded entirely by coinbase rewards we mine to its
    // own address below -- nothing pre-funded, nothing shared with other
    // test runs.
    let keypair = Keypair::generate();
    let address = keypair.encode_address(Network::Regtest);
    println!("burn-wallet regtest address: {address}");

    rpc.generate_to_address(BLOCKS_TO_MINE, &address)
        .expect("generatetoaddress failed");

    let tip = rpc.get_block_count().expect("getblockcount failed");
    assert_eq!(
        tip,
        starting_height + u64::from(BLOCKS_TO_MINE),
        "expected exactly {BLOCKS_TO_MINE} new blocks"
    );

    let utxos = find_spendable_coinbase(&rpc, &address).expect("coinbase UTXO discovery failed");
    assert!(
        !utxos.is_empty(),
        "expected at least one matured coinbase UTXO after mining {BLOCKS_TO_MINE} blocks to our own address"
    );
    let utxo = &utxos[0];
    println!(
        "spending coinbase UTXO: height={} value_zat={}",
        utxo.height, utxo.value_zat
    );

    let target_height = u32::try_from(tip).expect("regtest height fits in u32");
    let request = BurnTxRequest {
        network: Network::Regtest,
        target_height,
        utxos: vec![Utxo {
            outpoint: utxo.outpoint.clone(),
            value_zat: utxo.value_zat,
        }],
        change_and_signing_key: keypair,
        evm_address: EVM_ADDRESS,
        signal_bits: SIGNAL_BITS,
        burn_value_zat: BURN_VALUE_ZAT,
        fee_zat: FEE_ZAT,
        sova_ref: None,
    };
    let built = build_burn_transaction(&request).expect("build_burn_transaction failed");
    let raw_hex = hex::encode(&built.raw);
    let built_txid_rpc = encode_rpc_hash(built.txid);
    println!(
        "built burn tx: txid={built_txid_rpc} raw_len={}",
        built.raw.len()
    );

    // *** The empirical step: does Zebra's mempool/standardness policy
    // *** accept an OP_RETURN SIP-1 payload output plus a zero-hash P2PKH
    // *** ("eater") output? If not, this is a critical finding and the
    // *** node's exact error is captured verbatim below and this assertion
    // *** fails loudly with it.
    let node_txid = match rpc.send_raw_transaction(&raw_hex) {
        Ok(txid) => txid,
        Err(e) => panic!(
            "CRITICAL: sendrawtransaction REJECTED the SIP-1 burn transaction.\n\
             Verbatim node error: {e}\n\
             Raw tx hex ({} bytes): {raw_hex}",
            built.raw.len()
        ),
    };
    assert_eq!(
        node_txid, built_txid_rpc,
        "node-reported txid should match our computed txid"
    );

    rpc.generate(1).expect("mining the confirming block failed");

    let fetched = rpc
        .get_raw_transaction_verbose(&node_txid)
        .expect("getrawtransaction failed after mining");

    let confirmations = fetched
        .get("confirmations")
        .and_then(serde_json::Value::as_u64)
        .expect("verbose getrawtransaction should report confirmations");
    assert!(
        confirmations >= 1,
        "expected the burn tx to be confirmed, got: {fetched}"
    );

    let vout = fetched
        .get("vout")
        .and_then(serde_json::Value::as_array)
        .expect("verbose getrawtransaction should report vout");

    let decoded: Vec<(u64, Vec<u8>)> = vout
        .iter()
        .map(|out| {
            let value_zat = out
                .get("valueZat")
                .and_then(serde_json::Value::as_u64)
                .expect("vout[].valueZat");
            let script_hex = out
                .get("scriptPubKey")
                .and_then(|spk| spk.get("hex"))
                .and_then(serde_json::Value::as_str)
                .expect("vout[].scriptPubKey.hex");
            let script = hex::decode(script_hex).expect("scriptPubKey.hex should be valid hex");
            (value_zat, script)
        })
        .collect();

    // The exact check this whole test exists to perform: feed the
    // outputs Zebra actually confirmed back through crates/consensus's own
    // SIP-1 recognizer, and check they match what we built.
    let refs: Vec<TxOutRef<'_>> = decoded
        .iter()
        .map(|(value_zat, script)| TxOutRef {
            value_zat: *value_zat,
            script: script.as_slice(),
        })
        .collect();
    let burn = extract_burn(refs)
        .expect("consensus::sip1::extract_burn should recognize the confirmed outputs as a burn");

    assert_eq!(burn.evm_address, EVM_ADDRESS);
    assert_eq!(burn.signal_bits, SIGNAL_BITS);
    assert_eq!(burn.value_zat, BURN_VALUE_ZAT);

    println!(
        "e2e_regtest_burn PASSED: txid={node_txid} confirmations={confirmations} evm_address={} value_zat={}",
        hex::encode(burn.evm_address),
        burn.value_zat
    );
}
