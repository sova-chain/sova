//! The Sova NEAR DA index contract (`docs/design/near-da.md` §5).
//!
//! The batch bytes themselves live in NEAR's transaction history: each
//! `post` call's argument *is* one SOVADA1 batch. What this contract keeps
//! in state is the index a stranger needs to find and check every batch
//! without our servers: per batch, its height range, the sha256 of its
//! bytes, the hash of its last block, the NEAR block it executed in and
//! (filled in by a follow-up `set_tx`) the NEAR transaction that carried
//! it.
//!
//! `post` refuses anything that would make the archive wrong or ambiguous:
//! only the owner posts, the bytes must parse as SOVADA1 for this chain,
//! the batch must start exactly at `next_height` (so a range can never be
//! posted twice or skipped), every block's hash must be keccak-256 of its
//! header, every header's number must be its height, and every block's
//! parent must be the block before it, across batches too. The index is
//! therefore one hash-linked chain of Sova headers. Whether those blocks
//! are *valid Sova blocks* (seals, burns, mints) is for a Sova node with
//! its own zebrad to decide; this contract does not know Zcash.

use near_sdk::{AccountId, PanicOnDefault, env, near, require, store::Vector};

/// Most batches one `batches` view call returns.
pub const MAX_PAGE: u64 = 100;

/// One posted batch, as stored.
#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct Entry {
    first_height: u64,
    count: u32,
    bytes: u32,
    sha256: [u8; 32],
    last_hash: [u8; 32],
    near_block: u64,
    tx_hash: Option<[u8; 32]>,
}

/// One posted batch, as the view methods return it.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct BatchView {
    /// Position in the index (0 = first batch).
    pub index: u64,
    /// Height of the batch's first block.
    pub first_height: u64,
    /// Height of its last block.
    pub last_height: u64,
    /// Blocks in the batch.
    pub count: u32,
    /// Length of the batch (the function-call argument), in bytes.
    pub bytes: u32,
    /// sha256 of the batch bytes, hex.
    pub sha256: String,
    /// Hash of the batch's last Sova block, 0x-hex.
    pub last_hash: String,
    /// The NEAR block height the `post` receipt executed in. The carrying
    /// transaction is in that block or a few before it.
    pub near_block: u64,
    /// The NEAR transaction that carried the batch (base58), once the
    /// poster has recorded it with `set_tx`.
    pub tx_hash: Option<String>,
}

/// The archive's identity and progress.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// Batch format this index accepts.
    pub format: String,
    /// The only account allowed to post.
    pub owner: AccountId,
    /// EVM chain id of the archived Sova network.
    pub chain_id: u64,
    /// Height of the first archived block.
    pub start_height: u64,
    /// The height the next batch must start at (one past the archived tip).
    pub next_height: u64,
    /// Batches posted.
    pub batch_count: u64,
    /// Hash of the last archived block, 0x-hex.
    pub last_hash: Option<String>,
    /// Sum of all batch sizes, bytes.
    pub bytes_posted: u64,
}

/// Contract state.
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct SovaDa {
    owner: AccountId,
    chain_id: u64,
    start_height: u64,
    next_height: u64,
    last_hash: Option<[u8; 32]>,
    bytes_posted: u64,
    batches: Vector<Entry>,
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    s
}

fn view(index: u64, e: &Entry) -> BatchView {
    BatchView {
        index,
        first_height: e.first_height,
        last_height: e.first_height + u64::from(e.count) - 1,
        count: e.count,
        bytes: e.bytes,
        sha256: hex(&e.sha256),
        last_hash: format!("0x{}", hex(&e.last_hash)),
        near_block: e.near_block,
        tx_hash: e.tx_hash.map(|h| bs58::encode(h).into_string()),
    }
}

#[near]
impl SovaDa {
    /// Start an archive of chain `chain_id` whose first batch must begin at
    /// `start_height` (0 = from genesis), posted by `owner`.
    #[init]
    pub fn new(owner: AccountId, chain_id: u64, start_height: u64) -> Self {
        Self {
            owner,
            chain_id,
            start_height,
            next_height: start_height,
            last_hash: None,
            bytes_posted: 0,
            batches: Vector::new(b"b"),
        }
    }

    /// Record one batch. The call's raw argument bytes are the SOVADA1
    /// batch (not JSON). Owner only; see the module doc for every check.
    pub fn post(&mut self) {
        require!(
            env::predecessor_account_id() == self.owner,
            "only the owner posts"
        );
        let input = env::input().unwrap_or_default();
        let batch = sovada::parse(&input).unwrap_or_else(|e| env::panic_str(&e.to_string()));
        let h = batch.header;
        if h.chain_id != self.chain_id {
            env::panic_str(
                &sovada::Error::WrongChain {
                    expected: self.chain_id,
                    got: h.chain_id,
                }
                .to_string(),
            );
        }
        if h.first_height != self.next_height {
            env::panic_str(
                &sovada::Error::WrongStart {
                    expected: self.next_height,
                    got: h.first_height,
                }
                .to_string(),
            );
        }
        // Genesis has the all-zero parent; otherwise link to our last block.
        let prev = match (self.last_hash, h.first_height) {
            (Some(p), _) => Some(p),
            (None, 0) => Some([0u8; 32]),
            (None, _) => None,
        };
        let keccak = |d: &[u8]| env::keccak256_array(d);
        let last_hash = sovada::verify_blocks(&batch, &keccak, prev)
            .unwrap_or_else(|e| env::panic_str(&e.to_string()));
        let index = u64::from(self.batches.len());
        let bytes = u32::try_from(input.len()).unwrap_or(u32::MAX);
        let sha256 = env::sha256_array(&input);
        self.batches.push(Entry {
            first_height: h.first_height,
            count: h.count,
            bytes,
            sha256,
            last_hash,
            near_block: env::block_height(),
            tx_hash: None,
        });
        self.next_height = h.last_height() + 1;
        self.last_hash = Some(last_hash);
        self.bytes_posted += input.len() as u64;
        env::log_str(&format!(
            "EVENT_JSON:{{\"standard\":\"sovada\",\"version\":\"1.0.0\",\"event\":\"batch\",\"data\":[{{\"index\":{index},\"first_height\":{},\"count\":{},\"sha256\":\"{}\"}}]}}",
            h.first_height,
            h.count,
            hex(&sha256)
        ));
    }

    /// Record the NEAR transaction (base58 hash) that carried batch
    /// `index`. Owner only, once per batch. Readers check it: the
    /// transaction's argument must hash to the batch's `sha256`.
    pub fn set_tx(&mut self, index: u64, tx_hash: String) {
        require!(
            env::predecessor_account_id() == self.owner,
            "only the owner sets tx hashes"
        );
        let mut hash = [0u8; 32];
        let n = bs58::decode(tx_hash.as_bytes())
            .onto(&mut hash[..])
            .unwrap_or_else(|_| env::panic_str("tx_hash is not base58"));
        require!(n == 32, "tx_hash is not 32 bytes");
        let i = u32::try_from(index).unwrap_or(u32::MAX);
        let entry = self
            .batches
            .get_mut(i)
            .unwrap_or_else(|| env::panic_str("no such batch"));
        require!(entry.tx_hash.is_none(), "tx_hash already set");
        entry.tx_hash = Some(hash);
    }

    /// Hand posting to another account. Only the contract account itself
    /// (a full-access key on it) can call this.
    pub fn set_owner(&mut self, owner: AccountId) {
        require!(
            env::predecessor_account_id() == env::current_account_id(),
            "only the contract account changes the owner"
        );
        self.owner = owner;
    }

    /// The archive's identity and progress.
    pub fn info(&self) -> Info {
        Info {
            format: "SOVADA1".to_owned(),
            owner: self.owner.clone(),
            chain_id: self.chain_id,
            start_height: self.start_height,
            next_height: self.next_height,
            batch_count: u64::from(self.batches.len()),
            last_hash: self.last_hash.map(|h| format!("0x{}", hex(&h))),
            bytes_posted: self.bytes_posted,
        }
    }

    /// Up to `limit` (at most [`MAX_PAGE`]) batches from `from_index`.
    pub fn batches(&self, from_index: u64, limit: u64) -> Vec<BatchView> {
        let len = u64::from(self.batches.len());
        let end = from_index.saturating_add(limit.min(MAX_PAGE)).min(len);
        (from_index..end)
            .filter_map(|i| self.batches.get(u32::try_from(i).ok()?).map(|e| view(i, e)))
            .collect()
    }

    /// The batch containing Sova height `height`, if archived.
    pub fn find(&self, height: u64) -> Option<BatchView> {
        if height < self.start_height || height >= self.next_height {
            return None;
        }
        // Batches are contiguous and sorted: binary search on first_height.
        let (mut lo, mut hi) = (0u32, self.batches.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let e = self.batches.get(mid)?;
            if height < e.first_height {
                hi = mid;
            } else if height > e.first_height + u64::from(e.count) - 1 {
                lo = mid + 1;
            } else {
                return Some(view(u64::from(mid), e));
            }
        }
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;
    use tiny_keccak::{Hasher, Keccak};

    fn keccak(d: &[u8]) -> [u8; 32] {
        let mut k = Keccak::v256();
        k.update(d);
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    }

    fn rlp_bytes(p: &[u8]) -> Vec<u8> {
        if p.len() == 1 && p[0] < 0x80 {
            return p.to_vec();
        }
        let mut v = prefix(0x80, p.len());
        v.extend_from_slice(p);
        v
    }
    fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let p = items.concat();
        let mut v = prefix(0xc0, p.len());
        v.extend_from_slice(&p);
        v
    }
    fn prefix(base: u8, len: usize) -> Vec<u8> {
        if len < 56 {
            vec![base + len as u8]
        } else {
            let be = (len as u64).to_be_bytes();
            let s = be.iter().take_while(|&&b| b == 0).count();
            let mut v = vec![base + 55 + (8 - s) as u8];
            v.extend_from_slice(&be[s..]);
            v
        }
    }
    fn uint(n: u64) -> Vec<u8> {
        let be = n.to_be_bytes();
        let s = be.iter().take_while(|&&b| b == 0).count();
        rlp_bytes(&be[s..])
    }
    fn block(parent: [u8; 32], number: u64) -> (Vec<u8>, [u8; 32]) {
        let header = rlp_list(&[
            rlp_bytes(&parent),
            rlp_bytes(&[1; 32]),
            rlp_bytes(&[2; 20]),
            rlp_bytes(&[3; 32]),
            rlp_bytes(&[4; 32]),
            rlp_bytes(&[5; 32]),
            rlp_bytes(&[0; 256]),
            uint(0),
            uint(number),
            uint(30_000_000),
            uint(0),
            uint(1_790_000_000 + number),
            rlp_bytes(&[]),
        ]);
        let hash = keccak(&header);
        (rlp_list(&[header, rlp_list(&[]), rlp_list(&[])]), hash)
    }
    fn chain(first: u64, n: u64, parent: [u8; 32]) -> Vec<(u64, [u8; 32], Vec<u8>)> {
        let mut p = parent;
        (first..first + n)
            .map(|h| {
                let (raw, hash) = block(p, h);
                p = hash;
                (h, hash, raw)
            })
            .collect()
    }
    fn batch(chain_id: u64, c: &[(u64, [u8; 32], Vec<u8>)]) -> Vec<u8> {
        let blocks: Vec<_> = c
            .iter()
            .map(|(height, hash, raw)| sovada::Block {
                height: *height,
                hash: *hash,
                raw,
            })
            .collect();
        sovada::encode(chain_id, &blocks).unwrap()
    }

    fn owner() -> AccountId {
        "poster.testnet".parse().unwrap()
    }

    fn call(input: Vec<u8>, who: AccountId) {
        let mut ctx = VMContextBuilder::new()
            .current_account_id("sova-da.testnet".parse().unwrap())
            .predecessor_account_id(who.clone())
            .signer_account_id(who)
            .block_height(1234)
            .build();
        ctx.input = input.into();
        testing_env!(ctx);
    }

    #[test]
    fn posts_contiguous_linked_batches() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 82330, 0);
        let all = chain(0, 7, [0u8; 32]);
        let b1 = batch(82330, &all[..3]);
        call(b1.clone(), owner());
        c.post();
        call(batch(82330, &all[3..]), owner());
        c.post();
        let info = c.info();
        assert_eq!(info.next_height, 7);
        assert_eq!(info.batch_count, 2);
        assert_eq!(info.last_hash, Some(format!("0x{}", hex(&all[6].1))));
        let views = c.batches(0, 10);
        assert_eq!(views.len(), 2);
        assert_eq!((views[0].first_height, views[0].last_height), (0, 2));
        assert_eq!((views[1].first_height, views[1].last_height), (3, 6));
        assert_eq!(views[0].near_block, 1234);
        assert_eq!(views[0].bytes as usize, b1.len());
        assert_eq!(views[0].tx_hash, None);
        assert_eq!(c.find(4).unwrap().index, 1);
        assert_eq!(c.find(0).unwrap().index, 0);
        assert!(c.find(7).is_none());

        let tx = bs58::encode([7u8; 32]).into_string();
        call(vec![], owner());
        c.set_tx(1, tx.clone());
        assert_eq!(c.batches(1, 1)[0].tx_hash, Some(tx));
    }

    #[test]
    #[should_panic(expected = "tx_hash already set")]
    fn tx_hash_is_set_once() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 0);
        call(batch(1, &chain(0, 1, [0u8; 32])), owner());
        c.post();
        call(vec![], owner());
        c.set_tx(0, bs58::encode([1u8; 32]).into_string());
        c.set_tx(0, bs58::encode([2u8; 32]).into_string());
    }

    #[test]
    #[should_panic(expected = "batch starts at 0, expected 3")]
    fn refuses_a_double_post() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 0);
        let b = batch(1, &chain(0, 3, [0u8; 32]));
        call(b.clone(), owner());
        c.post();
        call(b, owner());
        c.post();
    }

    #[test]
    #[should_panic(expected = "batch starts at 4, expected 3")]
    fn refuses_a_gap() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 0);
        let all = chain(0, 6, [0u8; 32]);
        call(batch(1, &all[..3]), owner());
        c.post();
        call(batch(1, &all[4..]), owner());
        c.post();
    }

    #[test]
    #[should_panic(expected = "parentHash is not the previous block's hash")]
    fn refuses_a_fork_across_batches() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 10);
        let a = chain(10, 3, [9u8; 32]);
        call(batch(1, &a), owner());
        c.post();
        // Height 13 built on some other block 12.
        let other = chain(13, 2, [0xee; 32]);
        call(batch(1, &other), owner());
        c.post();
    }

    #[test]
    #[should_panic(expected = "only the owner posts")]
    fn refuses_strangers() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 0);
        call(
            batch(1, &chain(0, 1, [0u8; 32])),
            "mallory.testnet".parse().unwrap(),
        );
        c.post();
    }

    #[test]
    #[should_panic(expected = "chain id 2, expected 1")]
    fn refuses_another_chain() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 0);
        call(batch(2, &chain(0, 1, [0u8; 32])), owner());
        c.post();
    }

    #[test]
    #[should_panic(expected = "keccak(header) is not the claimed hash")]
    fn refuses_a_wrong_hash() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 0);
        let mut b = chain(0, 2, [0u8; 32]);
        b[1].1[5] ^= 1;
        call(batch(1, &b), owner());
        c.post();
    }

    #[test]
    #[should_panic(expected = "parentHash is not the previous block's hash")]
    fn genesis_must_have_the_zero_parent() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 0);
        call(batch(1, &chain(0, 1, [1u8; 32])), owner());
        c.post();
    }

    #[test]
    #[should_panic(expected = "only the contract account changes the owner")]
    fn owner_change_needs_the_contract_account() {
        call(vec![], owner());
        let mut c = SovaDa::new(owner(), 1, 0);
        c.set_owner("mallory.testnet".parse().unwrap());
    }
}
