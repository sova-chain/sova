//! The faucet's only persistent state: a small JSON file with per-address
//! and per-IP cooldowns, today's spend against the daily cap, and the
//! drips still in flight. The rate-limit rules live here too, as pure
//! functions of this state and an explicit clock, so they're unit-tested
//! without a node or a network.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Seconds per UTC day: the daily cap resets at 00:00 UTC.
pub(crate) const DAY_SECS: u64 = 24 * 60 * 60;

/// Errors reading or writing the state file.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StateError {
    #[error("state file I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("state file is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "state file belongs to faucet address {found}, but the keystore is {expected}; point state_file somewhere else"
    )]
    WrongFaucet { expected: String, found: String },
}

/// An outpoint in RPC display form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct OutPointRef {
    pub txid: String,
    pub vout: u32,
}

/// A drip broadcast but not yet seen mined. Its inputs are reserved (not
/// re-spent) until it confirms or expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingDrip {
    pub txid: String,
    pub recipient: String,
    pub amount_zat: u64,
    pub fee_zat: u64,
    pub spent: Vec<OutPointRef>,
    /// The transaction's expiry height: past it, a drip the node doesn't
    /// know can never be mined, and its inputs are free again.
    pub expiry_height: u64,
    pub sent_at: u64,
}

/// Why a drip was refused by the limits. Shown to users.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LimitRejection {
    #[error("this address already received a drip; try again in {retry_after_secs}s")]
    AddressCooldown { retry_after_secs: u64 },
    #[error("a drip was already sent to your network; try again in {retry_after_secs}s")]
    IpCooldown { retry_after_secs: u64 },
    #[error("the faucet's daily budget is spent; it resets at 00:00 UTC (in {retry_after_secs}s)")]
    DailyCapReached { retry_after_secs: u64 },
}

impl LimitRejection {
    pub(crate) fn retry_after_secs(&self) -> u64 {
        match self {
            Self::AddressCooldown { retry_after_secs }
            | Self::IpCooldown { retry_after_secs }
            | Self::DailyCapReached { retry_after_secs } => *retry_after_secs,
        }
    }
}

/// The limits the state enforces (a view of the config).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub address_cooldown_secs: u64,
    pub ip_cooldown_secs: u64,
    pub daily_cap_zat: u64,
}

/// The state file's contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FaucetState {
    pub version: u8,
    /// The faucet t-address this state belongs to (a guard against two
    /// keys sharing one state file).
    pub faucet_address: String,
    /// UTC day number (`unix_secs / 86400`) that `spent_today_zat` counts.
    pub day: u64,
    /// Drips + fees sent during `day`.
    pub spent_today_zat: u64,
    pub drips_today: u64,
    /// Canonical recipient t-addr -> unix time of its last drip.
    pub address_last_drip: BTreeMap<String, u64>,
    /// Client IP key (see [`ip_key`]) -> unix time of its last drip.
    pub ip_last_drip: BTreeMap<String, u64>,
    pub pending: Vec<PendingDrip>,
    pub total_drips: u64,
    pub total_sent_zat: u64,
}

impl FaucetState {
    pub(crate) fn new(faucet_address: String, now: u64) -> Self {
        Self {
            version: 1,
            faucet_address,
            day: now / DAY_SECS,
            spent_today_zat: 0,
            drips_today: 0,
            address_last_drip: BTreeMap::new(),
            ip_last_drip: BTreeMap::new(),
            pending: Vec::new(),
            total_drips: 0,
            total_sent_zat: 0,
        }
    }

    /// Loads the state at `path`, or starts fresh if the file doesn't exist.
    pub(crate) fn load_or_new(
        path: &Path,
        faucet_address: &str,
        now: u64,
    ) -> Result<Self, StateError> {
        if !path.exists() {
            return Ok(Self::new(faucet_address.to_string(), now));
        }
        let state: Self = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        if state.faucet_address != faucet_address {
            return Err(StateError::WrongFaucet {
                expected: faucet_address.to_string(),
                found: state.faucet_address,
            });
        }
        Ok(state)
    }

    /// Writes the state atomically (temp file + rename), so a crash never
    /// leaves a half-written file that would reset every cooldown.
    pub(crate) fn save(&self, path: &Path) -> Result<(), StateError> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        // It holds client IP prefixes: owner-only, like the keystore.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Resets the daily counter when the UTC day has changed.
    pub(crate) fn roll_day(&mut self, now: u64) {
        let today = now / DAY_SECS;
        if today != self.day {
            self.day = today;
            self.spent_today_zat = 0;
            self.drips_today = 0;
        }
    }

    /// Drops cooldown entries that no longer restrict anything, so the file
    /// (which holds client IP prefixes) keeps no more than it needs.
    pub(crate) fn prune(&mut self, limits: &Limits, now: u64) {
        self.address_last_drip
            .retain(|_, t| now < t.saturating_add(limits.address_cooldown_secs));
        self.ip_last_drip
            .retain(|_, t| now < t.saturating_add(limits.ip_cooldown_secs));
    }

    pub(crate) fn remaining_today_zat(&self, limits: &Limits) -> u64 {
        limits.daily_cap_zat.saturating_sub(self.spent_today_zat)
    }

    /// Checks the cooldowns and the daily cap for a drip costing `cost_zat`
    /// (amount + fee) to `address_key` from `ip`. Does not record anything.
    pub(crate) fn check(
        &mut self,
        limits: &Limits,
        address_key: &str,
        ip: &str,
        cost_zat: u64,
        now: u64,
    ) -> Result<(), LimitRejection> {
        self.roll_day(now);
        if let Some(retry_after_secs) = cooldown_left(
            self.address_last_drip.get(address_key),
            limits.address_cooldown_secs,
            now,
        ) {
            return Err(LimitRejection::AddressCooldown { retry_after_secs });
        }
        if let Some(retry_after_secs) =
            cooldown_left(self.ip_last_drip.get(ip), limits.ip_cooldown_secs, now)
        {
            return Err(LimitRejection::IpCooldown { retry_after_secs });
        }
        if self.spent_today_zat.saturating_add(cost_zat) > limits.daily_cap_zat {
            return Err(LimitRejection::DailyCapReached {
                retry_after_secs: DAY_SECS - now % DAY_SECS,
            });
        }
        Ok(())
    }

    /// Records a broadcast drip: starts both cooldowns, charges the daily
    /// budget, and reserves its inputs.
    pub(crate) fn record(&mut self, ip: &str, drip: PendingDrip, now: u64) {
        self.roll_day(now);
        let cost = drip.amount_zat.saturating_add(drip.fee_zat);
        self.address_last_drip.insert(drip.recipient.clone(), now);
        self.ip_last_drip.insert(ip.to_string(), now);
        self.spent_today_zat = self.spent_today_zat.saturating_add(cost);
        self.drips_today += 1;
        self.total_drips += 1;
        self.total_sent_zat = self.total_sent_zat.saturating_add(cost);
        self.pending.push(drip);
    }

    /// Every outpoint reserved by an in-flight drip.
    pub(crate) fn reserved(&self) -> std::collections::BTreeSet<OutPointRef> {
        self.pending
            .iter()
            .flat_map(|p| p.spent.iter().cloned())
            .collect()
    }
}

fn cooldown_left(last: Option<&u64>, cooldown_secs: u64, now: u64) -> Option<u64> {
    let until = last?.saturating_add(cooldown_secs);
    (now < until).then(|| until - now)
}

/// The per-IP cooldown key: an IPv4 address as-is; an IPv6 address by its
/// /64 (one subscriber usually holds a whole /64, so per-address IPv6
/// limits are trivially dodged); IPv4-mapped IPv6 as the IPv4 address.
pub(crate) fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4.to_string();
            }
            let s = v6.segments();
            let prefix = std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0);
            format!("{prefix}/64")
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        address_cooldown_secs: 3_600,
        ip_cooldown_secs: 600,
        daily_cap_zat: 30_000_000,
    };
    // 2026-09-22 12:00:00 UTC.
    const NOON: u64 = 1_790_078_400;

    fn drip(recipient: &str, amount: u64, fee: u64) -> PendingDrip {
        PendingDrip {
            txid: format!("{:0>64}", recipient.len()),
            recipient: recipient.to_string(),
            amount_zat: amount,
            fee_zat: fee,
            spent: vec![OutPointRef {
                txid: "aa".repeat(32),
                vout: 0,
            }],
            expiry_height: 140,
            sent_at: NOON,
        }
    }

    fn fresh() -> FaucetState {
        FaucetState::new("tmFaucet".into(), NOON)
    }

    #[test]
    fn address_cooldown_blocks_then_expires() {
        let mut s = fresh();
        s.check(&LIMITS, "tmA", "1.2.3.4", 10_010_000, NOON)
            .unwrap();
        s.record("1.2.3.4", drip("tmA", 10_000_000, 10_000), NOON);

        // Same address from a different IP: refused by the address cooldown.
        let err = s
            .check(&LIMITS, "tmA", "5.6.7.8", 10_010_000, NOON + 60)
            .unwrap_err();
        assert_eq!(
            err,
            LimitRejection::AddressCooldown {
                retry_after_secs: 3_540
            }
        );
        // After the cooldown, allowed again.
        s.check(&LIMITS, "tmA", "5.6.7.8", 10_010_000, NOON + 3_600)
            .unwrap();
    }

    #[test]
    fn ip_cooldown_blocks_other_addresses_from_same_ip() {
        let mut s = fresh();
        s.record("1.2.3.4", drip("tmA", 10_000_000, 10_000), NOON);
        let err = s
            .check(&LIMITS, "tmB", "1.2.3.4", 10_010_000, NOON + 1)
            .unwrap_err();
        assert_eq!(
            err,
            LimitRejection::IpCooldown {
                retry_after_secs: 599
            }
        );
        // A different IP is fine.
        s.check(&LIMITS, "tmB", "9.9.9.9", 10_010_000, NOON + 1)
            .unwrap();
        // The same IP after its (shorter) cooldown is fine.
        s.check(&LIMITS, "tmB", "1.2.3.4", 10_010_000, NOON + 600)
            .unwrap();
    }

    #[test]
    fn daily_cap_counts_amount_plus_fee_and_resets_at_utc_midnight() {
        let mut s = fresh();
        s.record("1.1.1.1", drip("tmA", 10_000_000, 10_000), NOON);
        s.record("2.2.2.2", drip("tmB", 10_000_000, 10_000), NOON);
        assert_eq!(s.spent_today_zat, 20_020_000);
        assert_eq!(s.remaining_today_zat(&LIMITS), 9_980_000);

        // A third 0.1 TAZ drip + fee would exceed the 0.3 TAZ cap.
        let err = s
            .check(&LIMITS, "tmC", "3.3.3.3", 10_010_000, NOON)
            .unwrap_err();
        assert_eq!(
            err,
            LimitRejection::DailyCapReached {
                retry_after_secs: 12 * 3_600
            }
        );
        // One second before midnight: still refused.
        assert!(
            s.check(&LIMITS, "tmC", "3.3.3.3", 10_010_000, NOON + 43_199)
                .is_err()
        );
        // At 00:00 UTC the budget resets.
        s.check(&LIMITS, "tmC", "3.3.3.3", 10_010_000, NOON + 43_200)
            .unwrap();
        assert_eq!(s.spent_today_zat, 0);
        assert_eq!(s.drips_today, 0);
        assert_eq!(s.total_drips, 2);
    }

    #[test]
    fn exact_cap_is_allowed() {
        let mut s = fresh();
        s.check(&LIMITS, "tmA", "1.1.1.1", LIMITS.daily_cap_zat, NOON)
            .unwrap();
        assert!(
            s.check(&LIMITS, "tmA", "1.1.1.1", LIMITS.daily_cap_zat + 1, NOON)
                .is_err()
        );
    }

    #[test]
    fn prune_drops_only_expired_cooldowns() {
        let mut s = fresh();
        s.record("1.1.1.1", drip("tmA", 1, 0), NOON);
        s.record("2.2.2.2", drip("tmB", 1, 0), NOON + 3_100);
        s.prune(&LIMITS, NOON + 3_600);
        assert!(!s.address_last_drip.contains_key("tmA"));
        assert!(s.address_last_drip.contains_key("tmB"));
        assert!(!s.ip_last_drip.contains_key("1.1.1.1"));
        assert!(s.ip_last_drip.contains_key("2.2.2.2"));
    }

    #[test]
    fn ipv6_is_grouped_by_64_and_mapped_v4_unwrapped() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(ip_key(a), "2001:db8:1:2::/64");
        assert_eq!(ip_key(a), ip_key(b));
        assert_ne!(ip_key(a), ip_key(c));
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert_eq!(ip_key(mapped), "192.0.2.7");
    }

    #[test]
    fn save_load_roundtrip_and_wrong_faucet_guard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = fresh();
        s.record("1.1.1.1", drip("tmA", 10_000_000, 10_000), NOON);
        s.save(&path).unwrap();
        let loaded = FaucetState::load_or_new(&path, "tmFaucet", NOON).unwrap();
        assert_eq!(loaded, s);
        // The cooldown survives a restart.
        let mut loaded = loaded;
        assert!(
            loaded
                .check(&LIMITS, "tmA", "8.8.8.8", 1, NOON + 5)
                .is_err()
        );
        assert!(matches!(
            FaucetState::load_or_new(&path, "tmOther", NOON),
            Err(StateError::WrongFaucet { .. })
        ));
    }
}
