//! Recipient address validation: transparent (P2PKH, P2SH, or ZIP-320 TEX)
//! addresses for the faucet's own network only.

use burn_wallet::Network;
use zcash_address::{ConversionError, TryFromAddress, ZcashAddress};
use zcash_protocol::consensus::{NetworkType, Parameters};
use zcash_transparent::address::TransparentAddress;

/// Longest input worth parsing (a unified address is ~200 chars; anything
/// we accept is well under 100).
const MAX_ADDRESS_LEN: usize = 256;

/// Why a recipient address was refused. Messages are shown to users.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum AddressError {
    #[error("address is empty")]
    Empty,
    #[error("address is too long")]
    TooLong,
    #[error("not a valid Zcash address")]
    Unparseable,
    #[error("address is for {actual}, but this faucet only pays {expected} addresses")]
    WrongNetwork { expected: String, actual: String },
    #[error(
        "{0} addresses are not supported: the faucet only pays transparent (t-) addresses, which is what sova-miner uses"
    )]
    NotTransparent(String),
    #[error("that is the faucet's own address")]
    FaucetItself,
}

/// A transparent payee, whatever encoding it arrived in.
struct Transparent(TransparentAddress);

impl TryFromAddress for Transparent {
    type Error = ();

    fn try_from_transparent_p2pkh(
        _net: NetworkType,
        data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self(TransparentAddress::PublicKeyHash(data)))
    }

    fn try_from_transparent_p2sh(
        _net: NetworkType,
        data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self(TransparentAddress::ScriptHash(data)))
    }

    /// ZIP-320 TEX: a P2PKH that must be paid from transparent inputs only,
    /// which every faucet transaction is.
    fn try_from_tex(
        _net: NetworkType,
        data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self(TransparentAddress::PublicKeyHash(data)))
    }
}

fn network_name(net: NetworkType) -> &'static str {
    match net {
        NetworkType::Main => "mainnet",
        NetworkType::Test => "testnet",
        NetworkType::Regtest => "regtest",
    }
}

/// A validated recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recipient {
    /// The address to pay.
    pub address: TransparentAddress,
    /// Its canonical t-address string on the faucet's network: the key
    /// for the per-address cooldown, so a TEX and the t-addr of the same
    /// key share one cooldown.
    pub canonical: String,
}

/// Validates `input` as a transparent address on `network` that isn't the
/// faucet's own (`faucet`).
pub(crate) fn validate_recipient(
    input: &str,
    network: Network,
    faucet: &TransparentAddress,
) -> Result<Recipient, AddressError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(AddressError::Empty);
    }
    if input.len() > MAX_ADDRESS_LEN {
        return Err(AddressError::TooLong);
    }
    let parsed = ZcashAddress::try_from_encoded(input).map_err(|_| AddressError::Unparseable)?;
    let net = network.network_type();
    let address = match parsed.convert_if_network::<Transparent>(net) {
        Ok(Transparent(a)) => a,
        Err(ConversionError::IncorrectNetwork { expected, actual }) => {
            return Err(AddressError::WrongNetwork {
                expected: network_name(expected).to_string(),
                actual: network_name(actual).to_string(),
            });
        }
        Err(ConversionError::Unsupported(kind)) => {
            return Err(AddressError::NotTransparent(kind.to_string()));
        }
        Err(ConversionError::User(())) => return Err(AddressError::Unparseable),
    };
    if &address == faucet {
        return Err(AddressError::FaucetItself);
    }
    let canonical = address.to_zcash_address(net).encode();
    Ok(Recipient { address, canonical })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use burn_wallet::Keypair;
    use zcash_address::ToAddress;
    use zcash_protocol::consensus::NetworkType;

    fn faucet() -> TransparentAddress {
        Keypair::generate().transparent_address()
    }

    #[test]
    fn accepts_testnet_p2pkh_and_p2sh() {
        let kp = Keypair::generate();
        let taddr = kp.encode_address(Network::Test);
        let r = validate_recipient(&taddr, Network::Test, &faucet()).unwrap();
        assert_eq!(r.address, kp.transparent_address());
        assert_eq!(r.canonical, taddr);

        let p2sh = ZcashAddress::from_transparent_p2sh(NetworkType::Test, [3u8; 20]).encode();
        assert!(p2sh.starts_with("t2"));
        let r = validate_recipient(&p2sh, Network::Test, &faucet()).unwrap();
        assert_eq!(r.address, TransparentAddress::ScriptHash([3u8; 20]));
    }

    #[test]
    fn regtest_accepts_testnet_encoded_t_addrs() {
        let kp = Keypair::generate();
        let taddr = kp.encode_address(Network::Test);
        assert!(validate_recipient(&taddr, Network::Regtest, &faucet()).is_ok());
    }

    #[test]
    fn tex_maps_to_the_same_cooldown_key_as_its_t_addr() {
        let kp = Keypair::generate();
        let TransparentAddress::PublicKeyHash(hash) = kp.transparent_address() else {
            unreachable!()
        };
        let tex = ZcashAddress::from_tex(NetworkType::Test, hash).encode();
        let r = validate_recipient(&tex, Network::Test, &faucet()).unwrap();
        assert_eq!(r.canonical, kp.encode_address(Network::Test));
    }

    #[test]
    fn refuses_mainnet_addresses() {
        let main = Keypair::generate().encode_address(Network::Main);
        assert!(main.starts_with("t1"));
        let err = validate_recipient(&main, Network::Test, &faucet()).unwrap_err();
        assert!(matches!(err, AddressError::WrongNetwork { .. }), "{err:?}");
        let err = validate_recipient(&main, Network::Regtest, &faucet()).unwrap_err();
        assert!(matches!(err, AddressError::WrongNetwork { .. }), "{err:?}");
    }

    #[test]
    fn refuses_shielded_addresses() {
        let sapling = ZcashAddress::from_sapling(NetworkType::Test, [7u8; 43]).encode();
        let err = validate_recipient(&sapling, Network::Test, &faucet()).unwrap_err();
        assert!(matches!(err, AddressError::NotTransparent(_)), "{err:?}");
    }

    #[test]
    fn refuses_garbage_empty_long_and_self() {
        let f = faucet();
        assert_eq!(
            validate_recipient("", Network::Test, &f),
            Err(AddressError::Empty)
        );
        assert_eq!(
            validate_recipient("0xdeadbeef", Network::Test, &f),
            Err(AddressError::Unparseable)
        );
        // A t-addr with a corrupted checksum.
        let mut bad = Keypair::generate().encode_address(Network::Test);
        let last = bad.pop().unwrap();
        bad.push(if last == 'a' { 'b' } else { 'a' });
        assert_eq!(
            validate_recipient(&bad, Network::Test, &f),
            Err(AddressError::Unparseable)
        );
        assert_eq!(
            validate_recipient(&"t".repeat(300), Network::Test, &f),
            Err(AddressError::TooLong)
        );
        let own = f.to_zcash_address(NetworkType::Test).encode();
        assert_eq!(
            validate_recipient(&own, Network::Test, &f),
            Err(AddressError::FaucetItself)
        );
    }
}
