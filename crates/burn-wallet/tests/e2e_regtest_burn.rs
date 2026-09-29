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
//! Every burn is signed for the consensus branch zebrad reports for its
//! next block (`getblockchaininfo.consensus.nextblock`), the way sova-miner
//! and sova-faucet sign (see `burn_wallet::branch`).
//!
//! Ignored by default (they need a live node). Run them via
//! `box/regtest/e2e-burn.sh`, or manually:
//!
//! ```text
//! cd box/regtest && docker compose up -d   # wait for it to report healthy
//! cd ../../crates/burn-wallet
//! cargo test --test e2e_regtest_burn -- --ignored --nocapture --exact e2e_regtest_burn
//! ```
//!
//! `BURN_WALLET_REGTEST_RPC` points them at another node (default
//! `http://127.0.0.1:18232`).
//!
//! `e2e_regtest_burn_across_nu7` needs an NU7-aware zebrad (Zebra's
//! `nu7-zips` branch, PR #11484, until a release carries NU7) on a *fresh*
//! regtest chain with NU5..NU6.3 at 1 and NU7 at
//! `BURN_WALLET_REGTEST_NU7_HEIGHT` (at least 110): see
//! `box/regtest/nu7-burn.sh`. It burns before, exactly at, and after the
//! activation height, and shows zebrad refusing a burn signed for the old
//! branch at the activation block.
// Integration test: an unexpected `Err`/`None` from the harness or the
// builder is exactly the failure this test exists to catch, and `.expect()`
// with a message naming the failing step is more useful here than a custom
// `Result`-returning test harness would be.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use burn_wallet::branch::{self, DEFAULT_TX_EXPIRY_DELTA, NU7_BRANCH_ID, NU7_TX_EXPIRY_DELTA};
use burn_wallet::rpc::RpcClient;
use burn_wallet::tx::{BuiltBurnTx, BurnTxRequest, Utxo, build_burn_transaction};
use burn_wallet::utxo::{SpendableUtxo, encode_rpc_hash, find_spendable_coinbase};
use burn_wallet::{Keypair, Network, NextBlockConsensus};
use consensus::sip1::{TxOutRef, extract_burn};
use zcash_protocol::consensus::BranchId;

const DEFAULT_RPC_URL: &str = "http://127.0.0.1:18232";
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

fn rpc() -> RpcClient {
    let url = std::env::var("BURN_WALLET_REGTEST_RPC").unwrap_or_else(|_| DEFAULT_RPC_URL.into());
    println!("zebrad RPC: {url}");
    RpcClient::new(url)
}

/// Builds and signs a burn of `utxo` for `consensus_branch_id` at
/// `next`'s height.
fn build(
    keypair: Keypair,
    utxo: &SpendableUtxo,
    next: &NextBlockConsensus,
    consensus_branch_id: u32,
) -> BuiltBurnTx {
    build_burn_transaction(&BurnTxRequest {
        network: Network::Regtest,
        target_height: next.next_height(),
        consensus_branch_id,
        expiry_delta: None,
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
    })
    .expect("build_burn_transaction failed")
}

/// Signs a burn of `utxo` for zebrad's next block, exactly as sova-miner
/// does, sends it, and checks the node took it. Returns the txid, the
/// snapshot it was signed against, and the built burn.
fn burn_for_next_block(
    rpc: &RpcClient,
    keypair: Keypair,
    utxo: &SpendableUtxo,
) -> (String, NextBlockConsensus, BuiltBurnTx) {
    let next = rpc
        .get_next_block_consensus()
        .expect("getblockchaininfo consensus");
    let built = build(keypair, utxo, &next, next.next_block_branch_id);
    let raw_hex = hex::encode(&built.raw);
    let built_txid_rpc = encode_rpc_hash(built.txid);
    println!(
        "built burn tx: txid={built_txid_rpc} raw_len={} next_block={} chaintip_branch={:08x} signed_for={} expiry_height={}",
        built.raw.len(),
        next.next_height(),
        next.chain_tip_branch_id,
        branch::describe(built.branch_id),
        built.expiry_height
    );
    assert_eq!(
        u32::from(built.branch_id),
        next.next_block_branch_id,
        "signed for zebrad's nextblock branch"
    );
    assert_eq!(
        u32::from_le_bytes(built.raw[8..12].try_into().unwrap()),
        next.next_block_branch_id,
        "the v5 header carries it"
    );

    // *** The empirical step: does Zebra's mempool/standardness policy
    // *** accept an OP_RETURN SIP-1 payload output plus a zero-hash P2PKH
    // *** ("eater") output, signed for the branch it reported? If not, this
    // *** is a critical finding and the node's exact error is captured
    // *** verbatim below and this assertion fails loudly with it.
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
    (node_txid, next, built)
}

/// Mines one block and checks `txid` is in it, with outputs that
/// `consensus::sip1::extract_burn` recognizes as the burn we built.
/// Returns the height it was mined at.
fn mine_and_verify(rpc: &RpcClient, txid: &str, expiry_height: u32) -> u64 {
    rpc.generate(1).expect("mining the confirming block failed");

    let fetched = rpc
        .get_raw_transaction_verbose(txid)
        .expect("getrawtransaction failed after mining");

    let confirmations = fetched
        .get("confirmations")
        .and_then(serde_json::Value::as_u64)
        .expect("verbose getrawtransaction should report confirmations");
    assert!(
        confirmations >= 1,
        "expected the burn tx to be confirmed, got: {fetched}"
    );
    let height = fetched
        .get("height")
        .and_then(serde_json::Value::as_u64)
        .expect("verbose getrawtransaction should report height");
    if let Some(expiry) = fetched
        .get("expiryheight")
        .and_then(serde_json::Value::as_u64)
    {
        assert_eq!(expiry, u64::from(expiry_height), "zebrad's expiryheight");
    }

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
    println!("burn {txid} mined at height {height} ({confirmations} confirmation(s))");
    height
}

#[test]
#[ignore = "requires the box/regtest docker harness; see box/regtest/e2e-burn.sh"]
fn e2e_regtest_burn() {
    let rpc = rpc();

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

    let (txid, next, built) = burn_for_next_block(&rpc, keypair, utxo);
    // Before NU7 the default expiry delta is unchanged.
    if !branch::is_nu7_or_later(built.branch_id) {
        assert_eq!(
            built.expiry_height,
            next.next_height() + DEFAULT_TX_EXPIRY_DELTA
        );
    }
    mine_and_verify(&rpc, &txid, built.expiry_height);
    println!(
        "e2e_regtest_burn PASSED: txid={txid} branch={}",
        branch::describe(built.branch_id)
    );
}

#[test]
#[ignore = "requires an NU7-aware zebrad regtest node; see box/regtest/nu7-burn.sh"]
fn e2e_regtest_burn_across_nu7() {
    let nu7: u64 = std::env::var("BURN_WALLET_REGTEST_NU7_HEIGHT")
        .expect("set BURN_WALLET_REGTEST_NU7_HEIGHT to the node's NU7 activation height")
        .parse()
        .expect("BURN_WALLET_REGTEST_NU7_HEIGHT is a height");
    let rpc = rpc();
    let start = rpc.get_block_count().expect("zebrad RPC unreachable");
    assert!(
        nu7 >= 110 && start + 3 < nu7,
        "needs a fresh chain well below NU7 (tip {start}, NU7 at {nu7}): 101 blocks of coinbase maturity first"
    );

    let keypair = Keypair::generate();
    let address = keypair.encode_address(Network::Regtest);
    // Tip at NU7 - 3: every coinbase up to NU7 - 102 is mature, one per
    // burn below.
    let to_mine = u32::try_from(nu7 - 3 - start).unwrap();
    rpc.generate_to_address(to_mine, &address)
        .expect("generatetoaddress failed");
    let utxos = find_spendable_coinbase(&rpc, &address).expect("coinbase UTXO discovery failed");
    assert!(utxos.len() >= 3, "need three mature coinbase outputs");

    // 1. Before NU7: next block NU7 - 2 is still on the old branch.
    let (txid, next, built) = burn_for_next_block(&rpc, keypair, &utxos[0]);
    assert_eq!(u64::from(next.next_height()), nu7 - 2);
    assert_ne!(next.next_block_branch_id, NU7_BRANCH_ID);
    assert!(!branch::is_nu7_or_later(built.branch_id));
    assert_eq!(
        built.expiry_height,
        next.next_height() + DEFAULT_TX_EXPIRY_DELTA
    );
    assert_eq!(mine_and_verify(&rpc, &txid, built.expiry_height), nu7 - 2);
    rpc.generate(1).expect("mining to NU7 - 1 failed");

    // 2. The activation block: tip NU7 - 1 is on the old branch, zebrad's
    //    next block is NU7's.
    let next = rpc.get_next_block_consensus().unwrap();
    assert_eq!(u64::from(next.next_height()), nu7);
    assert!(next.next_block_activates_upgrade());
    assert_eq!(next.next_block_branch_id, NU7_BRANCH_ID);
    //    A burn signed the way sova-miner used to (for the tip's branch,
    //    which `BranchId::for_height` with no NU7 height also gives) is
    //    refused by zebrad: the ZIP 244 sighash commits to the branch.
    let stale = build(keypair, &utxos[1], &next, next.chain_tip_branch_id);
    let refused = rpc.send_raw_transaction(&hex::encode(&stale.raw));
    println!("burn signed for the pre-NU7 branch at the activation block: {refused:?}");
    assert!(
        refused.is_err(),
        "zebrad must refuse a burn signed for the old branch after NU7"
    );
    //    The same coin, signed for zebrad's `nextblock`, is taken and mined
    //    in the activation block itself.
    let (txid, _, built) = burn_for_next_block(&rpc, keypair, &utxos[1]);
    assert_eq!(built.branch_id, BranchId::Nu7);
    assert_eq!(
        u64::from(built.expiry_height),
        nu7 + u64::from(NU7_TX_EXPIRY_DELTA)
    );
    assert_eq!(mine_and_verify(&rpc, &txid, built.expiry_height), nu7);

    // 3. After NU7: tip and next block both NU7.
    let (txid, next, built) = burn_for_next_block(&rpc, keypair, &utxos[2]);
    assert_eq!(next.chain_tip_branch_id, NU7_BRANCH_ID);
    assert_eq!(built.branch_id, BranchId::Nu7);
    assert_eq!(mine_and_verify(&rpc, &txid, built.expiry_height), nu7 + 1);
    println!(
        "e2e_regtest_burn_across_nu7 PASSED: burns mined at {}, {nu7} (the activation block) and {}",
        nu7 - 2,
        nu7 + 1
    );
}
