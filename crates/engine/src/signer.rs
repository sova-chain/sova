//! SIP-6 §3 and §2.7: the node's sealing key and its seal journal.
//!
//! The seal key is the key of the EVM address the miner's burns credit — the
//! same `sova-miner` keystore (`{"version": 1, "secret_key_hex": …}`), read
//! here directly because `bin/sova` cannot link the burn-wallet crate.
//!
//! **The journal** keeps an honest sealer from equivocating by accident: a
//! retried build (a re-fired trigger whose first block was signed and relayed
//! but whose forkchoice update failed) would otherwise sign a second block
//! for the same slot. Before a signed block is released, it is written to the
//! journal and fsynced; asked to seal the same slot again, the signer returns
//! the journaled block instead of signing a new one. A slot is
//! `(number, parent_hash, parent_beacon_block_root)`: a re-seal on a new
//! parent (a late-win reorg) or a new anchor (a Zcash reorg) is a new slot.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use alloy_primitives::{Address, B256};
use alloy_rlp::{Decodable, Encodable};
use reth_ethereum::{Block, primitives::SealedBlock};

use crate::seal::{self, SealError};

/// How many journaled slots to keep (oldest heights pruned first).
const JOURNAL_KEEP: usize = 1024;

/// Why the sealer could not seal.
#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    /// The keystore could not be read or parsed.
    #[error("sealer keystore {path}: {reason}")]
    Keystore {
        /// The keystore path.
        path: PathBuf,
        /// What was wrong.
        reason: String,
    },
    /// Signing failed.
    #[error(transparent)]
    Seal(#[from] SealError),
    /// The journal could not be written or read.
    #[error("seal journal: {0}")]
    Journal(String),
}

/// The node's sealing key, chain and journal.
#[derive(Debug)]
pub struct Signer {
    secret: B256,
    address: Address,
    chain_id: u64,
    journal: PathBuf,
    vanity: Vec<u8>,
}

impl Signer {
    /// Load the key from a `sova-miner` keystore and open (create) the
    /// journal directory.
    ///
    /// # Errors
    /// A missing or malformed keystore, an invalid key, or an unwritable
    /// journal directory.
    pub fn from_keystore(
        keystore: &Path,
        chain_id: u64,
        journal: PathBuf,
    ) -> Result<Self, SignerError> {
        let bad = |reason: String| SignerError::Keystore {
            path: keystore.to_path_buf(),
            reason,
        };
        let text = fs::read_to_string(keystore).map_err(|e| bad(e.to_string()))?;
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| bad(e.to_string()))?;
        let hex = json
            .get("secret_key_hex")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| bad("no secret_key_hex".into()))?;
        let bytes = alloy_primitives::hex::decode(hex).map_err(|e| bad(e.to_string()))?;
        let secret =
            B256::try_from(bytes.as_slice()).map_err(|_| bad("secret is not 32 bytes".into()))?;
        Self::new(secret, chain_id, journal)
    }

    /// A signer over `secret` (tests, tools).
    ///
    /// # Errors
    /// An invalid key or an unwritable journal directory.
    pub fn new(secret: B256, chain_id: u64, journal: PathBuf) -> Result<Self, SignerError> {
        let address = seal::address_of(secret)?;
        fs::create_dir_all(&journal).map_err(|e| SignerError::Journal(e.to_string()))?;
        Ok(Self {
            secret,
            address,
            chain_id,
            journal,
            vanity: b"sova".to_vec(),
        })
    }

    /// The address this signer seals as (the burn's credited address).
    #[must_use]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Seal `block`, or return the block already journaled for its slot.
    ///
    /// # Errors
    /// Signing or journal I/O failed; nothing is released then.
    pub fn seal(&self, block: SealedBlock<Block>) -> Result<SealedBlock<Block>, SignerError> {
        let path = self.slot_path(&block);
        if let Some(journaled) = self.read(&path)? {
            if journaled.hash() != block.hash() {
                tracing::warn!(
                    height = block.number,
                    journaled = %journaled.hash(),
                    "seal journal: slot already signed; re-publishing that block, not signing another"
                );
            }
            return Ok(journaled);
        }
        let mut block = block.into_block();
        seal::sign(&mut block.header, &self.vanity, self.secret, self.chain_id)?;
        let sealed = SealedBlock::seal_slow(block);
        self.write(&path, &sealed)?;
        self.prune();
        Ok(sealed)
    }

    /// Drop the journal entry for `block`'s slot when it holds exactly
    /// `block` and our own engine rejected that block as permanently invalid
    /// (not a hold). SIP-6 §2.7 counts only two *valid* headers for one slot
    /// as equivocation, and a block that failed validation for good can
    /// never become valid, so signing a new block for the slot is honest.
    /// Without this the signer re-published the invalid block on every retry
    /// and the node could never seal that slot again (public testnet
    /// 2026-09-26: a state-root mismatch at height 6225 after a Zcash reorg
    /// flip-flop stalled the chain). The entry is moved to `invalid/` as a
    /// record, not deleted. Returns whether an entry was dropped.
    ///
    /// # Errors
    /// Journal I/O failed.
    pub fn discard_invalid(&self, block: &SealedBlock<Block>) -> Result<bool, SignerError> {
        let path = self.slot_path(block);
        let Some(journaled) = self.read(&path)? else {
            return Ok(false);
        };
        if journaled.hash() != block.hash() {
            return Ok(false);
        }
        let io = |e: std::io::Error| SignerError::Journal(e.to_string());
        let dir = self.journal.join("invalid");
        fs::create_dir_all(&dir).map_err(io)?;
        let name = path
            .file_name()
            .ok_or_else(|| SignerError::Journal("journal entry has no name".into()))?;
        fs::rename(&path, dir.join(name)).map_err(io)?;
        fs::File::open(&self.journal)
            .and_then(|d| d.sync_all())
            .map_err(io)?;
        Ok(true)
    }

    fn slot_path(&self, block: &SealedBlock<Block>) -> PathBuf {
        let anchor = block.header().parent_beacon_block_root.unwrap_or_default();
        self.journal.join(format!(
            "{:012}-{}-{}.rlp",
            block.number,
            alloy_primitives::hex::encode(block.parent_hash),
            alloy_primitives::hex::encode(anchor)
        ))
    }

    fn read(&self, path: &Path) -> Result<Option<SealedBlock<Block>>, SignerError> {
        match fs::read(path) {
            Ok(bytes) => Block::decode(&mut bytes.as_slice())
                .map(|b| Some(SealedBlock::seal_slow(b)))
                .map_err(|e| SignerError::Journal(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SignerError::Journal(e.to_string())),
        }
    }

    /// Write-then-rename with fsyncs, so a crash leaves either no entry or
    /// a whole one — never a signature released without its record.
    fn write(&self, path: &Path, block: &SealedBlock<Block>) -> Result<(), SignerError> {
        let io = |e: std::io::Error| SignerError::Journal(e.to_string());
        let mut rlp = Vec::new();
        block.clone_block().encode(&mut rlp);
        let tmp = path.with_extension("tmp");
        let mut f = fs::File::create(&tmp).map_err(io)?;
        f.write_all(&rlp).map_err(io)?;
        f.sync_all().map_err(io)?;
        fs::rename(&tmp, path).map_err(io)?;
        fs::File::open(&self.journal)
            .and_then(|d| d.sync_all())
            .map_err(io)?;
        Ok(())
    }

    fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.journal) else {
            return;
        };
        let mut names: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rlp"))
            .collect();
        if names.len() <= JOURNAL_KEEP {
            return;
        }
        // Zero-padded heights sort by name.
        names.sort();
        for old in &names[..names.len() - JOURNAL_KEEP] {
            let _ = fs::remove_file(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("sova-seal-journal-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn block(parent: u8, ts: u64) -> SealedBlock<Block> {
        let mut b = Block::default();
        b.header.number = 9;
        b.header.parent_hash = B256::repeat_byte(parent);
        b.header.timestamp = ts;
        // Optional header fields are encoded in order, so everything
        // before the anchor must be present (as in every real block).
        b.header.base_fee_per_gas = Some(7);
        b.header.withdrawals_root = Some(crate::seal::EMPTY_ROOT);
        b.header.blob_gas_used = Some(0);
        b.header.excess_blob_gas = Some(0);
        b.header.parent_beacon_block_root = Some(B256::repeat_byte(0x5A));
        b.body.withdrawals = Some(Vec::new().into());
        SealedBlock::seal_slow(b)
    }

    #[test]
    fn a_signed_slot_is_never_signed_twice() {
        let d = dir("twice");
        let s = Signer::new(B256::repeat_byte(0x42), 82_330, d.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        let first = s.seal(block(1, 100)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            seal::recover(first.header(), 82_330).ok(),
            Some(s.address())
        );
        // Same slot, a different build (new timestamp): the journaled block.
        let again = s.seal(block(1, 101)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(again.hash(), first.hash());
        // A restart keeps the journal.
        let s2 = Signer::new(B256::repeat_byte(0x42), 82_330, d.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            s2.seal(block(1, 102)).map(|b| b.hash()).ok(),
            Some(first.hash())
        );
        // A new parent is a new slot.
        let other = s.seal(block(2, 100)).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(other.hash(), first.hash());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_permanently_invalid_journaled_block_can_be_resigned() {
        let d = dir("invalid");
        let s = Signer::new(B256::repeat_byte(0x42), 82_330, d.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        let first = s.seal(block(1, 100)).unwrap_or_else(|e| panic!("{e}"));
        // Another block for the same slot is not the journaled one: kept.
        assert_eq!(s.discard_invalid(&block(1, 999)).ok(), Some(false));
        assert_eq!(
            s.seal(block(1, 101)).map(|b| b.hash()).ok(),
            Some(first.hash())
        );
        // Discarding the journaled block itself frees the slot: the next
        // build for it is signed afresh (a different block, same signer).
        assert_eq!(s.discard_invalid(&first).ok(), Some(true));
        let fresh = s.seal(block(1, 101)).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(fresh.hash(), first.hash());
        assert_eq!(
            seal::recover(fresh.header(), 82_330).ok(),
            Some(s.address())
        );
        // The discarded entry is kept aside; the fresh one is journaled.
        assert!(
            d.join("invalid")
                .read_dir()
                .is_ok_and(|mut r| r.next().is_some())
        );
        assert_eq!(
            s.seal(block(1, 102)).map(|b| b.hash()).ok(),
            Some(fresh.hash())
        );
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn the_keystore_format_of_sova_miner_loads() {
        let d = dir("keystore");
        let _ = fs::create_dir_all(&d);
        let ks = d.join("keystore.json");
        let _ = fs::write(
            &ks,
            format!(
                r#"{{"version": 1, "secret_key_hex": "{}"}}"#,
                "42".repeat(32)
            ),
        );
        let s =
            Signer::from_keystore(&ks, 82_330, d.join("journal")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            s.address(),
            alloy_primitives::address!("17c5185167401ed00cf5f5b2fc97d9bbfdb7d025")
        );
        let _ = fs::write(&ks, r#"{"version": 1}"#);
        assert!(Signer::from_keystore(&ks, 82_330, d.join("journal")).is_err());
        let _ = fs::remove_dir_all(d);
    }
}
