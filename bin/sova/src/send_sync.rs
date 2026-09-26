//! The `eth_sendRawTransactionSync` timeout (EIP-7966): how long the node
//! holds the call open waiting for the transaction's block.
//!
//! reth defaults to 30 s. A Sova block follows each Zcash block (~75 s
//! target; the public testnet measured a mean of ~49 s and a p90 of
//! ~3 min, and a 5-minute gap is normal), so with reth's default most sync
//! sends would time out before their block. Sova's default is
//! [`DEFAULT_SEND_SYNC_TIMEOUT`] (300 s); `SOVA_SEND_SYNC_TIMEOUT_SECS`
//! overrides it (a whole number of seconds, 1 to [`MAX_SEND_SYNC_TIMEOUT_SECS`]).
//! A caller's own `timeout_ms` (the method's optional second parameter)
//! can only shorten it: reth takes the smaller of the two.
//!
//! **Why an `EthApiBuilder`, not just `RpcServerArgs`.** reth v2.6.0 has
//! the flag (`--rpc.send-raw-transaction-sync-timeout`,
//! `RpcServerArgs::rpc_send_raw_transaction_sync_timeout`) but never
//! passes it on: `RethRpcServerConfig::eth_config` leaves it out, and
//! `EthApiCtx::eth_api_builder` doesn't set it, so every node answers with
//! the builder's hard-coded 30 s whatever the flag says. [`SovaEthApiBuilder`]
//! is reth's `EthereumEthApiBuilder` plus that one setting, and
//! [`add_ons`] is `SovaNodeAddOns` with it swapped in. The value is also
//! written to `RpcServerArgs` ([`apply`]) so the node config tells the
//! truth, and a later reth that wires the flag through gets the same value.
//!
//! **Through the public RPC** (`rpc.testnet.sova.io`, the Cloudflare
//! Worker in `infra/testnet/worker/rpc-firewall.mjs`), Cloudflare cuts a
//! proxied request that hasn't answered in ~100 s (HTTP 524). The Worker
//! therefore clamps the call's `timeout_ms` to 90 s, so a slow block comes
//! back as reth's JSON-RPC timeout error (with the transaction hash)
//! instead of a bare 524. The effective limit there is 90 s; the full
//! 300 s applies to a node you reach directly (your own, or the box).

use std::time::Duration;

use alloy_network::Ethereum;
use engine::SovaEngineValidatorBuilder;
use reth_ethereum::{
    chainspec::{EthereumHardforks, Hardforks},
    evm::primitives::ConfigureEvm,
    node::{
        api::{FullNodeComponents, HeaderTy, NodeTypes, PrimitivesTy, TxTy},
        builder::rpc::{
            BasicEngineApiBuilder, BasicEngineValidatorBuilder, EthApiBuilder, EthApiCtx, Identity,
            RpcAddOns,
        },
        core::args::RpcServerArgs,
    },
    rpc::eth::{
        EthApiError,
        core::{EthApiFor, EthRpcConverterFor},
        error::FromEvmError,
    },
};
use reth_rpc_eth_api::{
    RpcConvert, RpcTypes, SignableTxRequest, helpers::pending_block::BuildPendingEnv,
};

/// The environment variable that overrides [`DEFAULT_SEND_SYNC_TIMEOUT`].
pub(crate) const ENV: &str = "SOVA_SEND_SYNC_TIMEOUT_SECS";

/// Sova's default: several median blocks, and past the normal 5-minute gap.
pub(crate) const DEFAULT_SEND_SYNC_TIMEOUT: Duration = Duration::from_secs(300);

/// The largest override accepted (1 h): past that, a client should poll
/// `eth_getTransactionReceipt` rather than hold a connection open.
pub(crate) const MAX_SEND_SYNC_TIMEOUT_SECS: u64 = 3600;

/// Parse a `SOVA_SEND_SYNC_TIMEOUT_SECS` value; unset or empty means the
/// default. Anything else must be whole seconds in 1..=3600, so a typo
/// never starts a node with a surprising timeout.
pub(crate) fn parse(raw: Option<&str>) -> eyre::Result<Duration> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_SEND_SYNC_TIMEOUT);
    };
    match raw.parse::<u64>() {
        Ok(secs) if (1..=MAX_SEND_SYNC_TIMEOUT_SECS).contains(&secs) => {
            Ok(Duration::from_secs(secs))
        }
        _ => Err(eyre::eyre!(
            "{ENV} must be whole seconds from 1 to {MAX_SEND_SYNC_TIMEOUT_SECS}, got {raw:?}"
        )),
    }
}

/// Read the timeout from [`ENV`].
pub(crate) fn from_env() -> eyre::Result<Duration> {
    parse(std::env::var(ENV).ok().as_deref())
}

/// Record the timeout in reth's RPC server args (see the module doc: reth
/// v2.6.0 doesn't read it from there, [`SovaEthApiBuilder`] applies it).
pub(crate) const fn apply(rpc: &mut RpcServerArgs, timeout: Duration) {
    rpc.rpc_send_raw_transaction_sync_timeout = timeout;
}

/// reth's `EthereumEthApiBuilder` with the sync-send timeout set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SovaEthApiBuilder {
    timeout: Duration,
}

impl SovaEthApiBuilder {
    pub(crate) const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for SovaEthApiBuilder {
    fn default() -> Self {
        Self::new(DEFAULT_SEND_SYNC_TIMEOUT)
    }
}

// Bounds copied from reth v2.6.0's `EthereumEthApiBuilder<Ethereum>`.
impl<N> EthApiBuilder<N> for SovaEthApiBuilder
where
    N: FullNodeComponents<
            Types: NodeTypes<ChainSpec: Hardforks + EthereumHardforks>,
            Evm: ConfigureEvm<NextBlockEnvCtx: BuildPendingEnv<HeaderTy<N::Types>>>,
        >,
    Ethereum: RpcTypes<TransactionRequest: SignableTxRequest<TxTy<N::Types>>>,
    EthRpcConverterFor<N>: RpcConvert<
            Primitives = PrimitivesTy<N::Types>,
            Error = EthApiError,
            Network = Ethereum,
            Evm = N::Evm,
        >,
    EthApiError: FromEvmError<N::Evm>,
{
    type EthApi = EthApiFor<N>;

    async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
        Ok(ctx
            .eth_api_builder()
            .send_raw_transaction_sync_timeout(self.timeout)
            .build())
    }
}

/// `engine::SovaNodeAddOns` (what `SovaNode::add_ons` returns) with
/// [`SovaEthApiBuilder`] in place of `EthereumEthApiBuilder`; every other
/// part is the same default.
pub(crate) fn add_ons<N>(
    timeout: Duration,
) -> RpcAddOns<N, SovaEthApiBuilder, SovaEngineValidatorBuilder>
where
    N: FullNodeComponents,
    SovaEthApiBuilder: EthApiBuilder<N>,
{
    RpcAddOns::new(
        SovaEthApiBuilder::new(timeout),
        SovaEngineValidatorBuilder::default(),
        BasicEngineApiBuilder::default(),
        BasicEngineValidatorBuilder::default(),
        Identity::new(),
        Identity::new(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_is_the_sova_default() {
        assert_eq!(parse(None).unwrap(), Duration::from_secs(300));
        assert_eq!(parse(Some("")).unwrap(), DEFAULT_SEND_SYNC_TIMEOUT);
        assert_eq!(parse(Some("  ")).unwrap(), DEFAULT_SEND_SYNC_TIMEOUT);
        assert_eq!(
            SovaEthApiBuilder::default().timeout,
            DEFAULT_SEND_SYNC_TIMEOUT
        );
    }

    #[test]
    fn the_default_outlasts_a_slow_sova_block() {
        // reth's own 30 s is shorter than a median Sova block; ours must
        // cover the normal 5-minute gap.
        let reth_default = RpcServerArgs::default().rpc_send_raw_transaction_sync_timeout;
        assert_eq!(reth_default, Duration::from_secs(30));
        assert!(DEFAULT_SEND_SYNC_TIMEOUT >= Duration::from_secs(5 * 60));
    }

    #[test]
    fn override_is_whole_seconds_in_range() {
        assert_eq!(parse(Some("90")).unwrap(), Duration::from_secs(90));
        assert_eq!(parse(Some(" 600 ")).unwrap(), Duration::from_secs(600));
        assert_eq!(parse(Some("1")).unwrap(), Duration::from_secs(1));
        assert_eq!(parse(Some("3600")).unwrap(), Duration::from_secs(3600));
        for bad in ["0", "3601", "-5", "1.5", "5m", "300s", "abc"] {
            let err = parse(Some(bad)).unwrap_err().to_string();
            assert!(err.contains(ENV), "{bad}: {err}");
        }
    }

    #[test]
    fn apply_sets_reths_field() {
        let mut rpc = RpcServerArgs::default();
        apply(&mut rpc, Duration::from_secs(123));
        assert_eq!(
            rpc.rpc_send_raw_transaction_sync_timeout,
            Duration::from_secs(123)
        );
        assert_eq!(
            SovaEthApiBuilder::new(Duration::from_secs(123)).timeout,
            Duration::from_secs(123)
        );
    }
}
