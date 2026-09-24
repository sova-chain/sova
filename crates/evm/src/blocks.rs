//! SIP-7 §4.1: the `ZcashBlocks` pre-block system call.
//!
//! [`SovaEvmConfig`] is reth's Ethereum EVM config over
//! [`crate::zcash::SovaEvmFactory`] with one change: its block executor,
//! after the standard pre-execution system calls (EIP-4788, EIP-2935) and
//! before the first transaction, records Zcash block `E_N`'s summary in the
//! `ZcashBlocks` contract at [`ZCASH_BLOCKS`] — EIP-4788's pattern: caller
//! [`SYSTEM_ADDRESS`], value 0, gas not counted against the block, state
//! committed. Only when SIP-7 is active and never at genesis.
//!
//! The calldata comes from the same index record the precompile serves
//! (`encode_zcash_blocks_record`), under the same guards: a missing record
//! or a failed call refuses the block as an execution error — not cached as
//! invalid, retried — like the precompile's fatal (SIP-4 §5). An honest
//! node's record for an anchored, scanned epoch is always present (the
//! follower holds instead of skipping), so a refusal means a local fault.
//!
//! The payload builder, the importer and the RPC all execute through this
//! one config, so the sealer and every validator compute the same write.

use std::sync::Arc;

use reth_ethereum::{
    Block, EthPrimitives, Receipt, TransactionSigned, TxType,
    chainspec::ChainSpec,
    evm::{
        EthBlockAssembler, EthEvm, EthEvmConfig, RethReceiptBuilder,
        primitives::{
            Evm, EvmEnv, EvmEnvFor, EvmFactory, ExecutionCtxFor, InspectorFor,
            NextBlockEnvAttributes,
            block::{BlockExecutorFactory, ExecutableTx, GasOutput, StateDB},
            eth::{EthBlockExecutionCtx, EthBlockExecutor, EthTxResult},
            execute::{BlockExecutionError, BlockExecutor, InternalBlockExecutionError},
            precompiles::PrecompilesMap,
        },
        revm::{
            DatabaseCommit,
            context::{Block as _, TxEnv},
            primitives::hardfork::SpecId,
        },
    },
    node::api::{ConfigureEngineEvm, ConfigureEvm, ExecutableTxIterator},
    primitives::{Header, SealedBlock, SealedHeader},
    provider::BlockExecutionResult,
    rpc::types::engine::ExecutionData,
};

use crate::zcash::{
    SYSTEM_ADDRESS, SovaEvmFactory, ZCASH_BLOCKS, anchored_height, encode_zcash_blocks_record,
    sip7_active, zcash_source,
};

/// Reth's Ethereum EVM config over [`SovaEvmFactory`], with the SIP-7
/// `ZcashBlocks` pre-block call in its block executor.
#[derive(Debug, Clone)]
pub struct SovaEvmConfig {
    inner: EthEvmConfig<ChainSpec, SovaEvmFactory>,
}

impl SovaEvmConfig {
    /// Wrap reth's config.
    #[must_use]
    pub const fn new(inner: EthEvmConfig<ChainSpec, SovaEvmFactory>) -> Self {
        Self { inner }
    }
}

impl BlockExecutorFactory for SovaEvmConfig {
    type EvmFactory = SovaEvmFactory;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type TxExecutionResult = EthTxResult<<SovaEvmFactory as EvmFactory>::HaltReason, TxType>;
    type Executor<'a, DB: StateDB, I: InspectorFor<Self, DB>> =
        SovaBlockExecutor<'a, EthEvm<DB, I, PrecompilesMap>>;

    fn evm_factory(&self) -> &Self::EvmFactory {
        self.inner.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EthEvm<DB, I, PrecompilesMap>,
        ctx: EthBlockExecutionCtx<'a>,
    ) -> Self::Executor<'a, DB, I>
    where
        DB: StateDB,
        I: InspectorFor<Self, DB>,
    {
        SovaBlockExecutor {
            inner: EthBlockExecutor::new(
                evm,
                ctx,
                self.inner.chain_spec(),
                self.inner.executor_factory.receipt_builder(),
            ),
        }
    }
}

impl ConfigureEvm for SovaEvmConfig {
    type Primitives = EthPrimitives;
    type Error = <EthEvmConfig<ChainSpec, SovaEvmFactory> as ConfigureEvm>::Error;
    type NextBlockEnvCtx = NextBlockEnvAttributes;
    type BlockExecutorFactory = Self;
    type BlockAssembler = EthBlockAssembler<ChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        self
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.inner.block_assembler()
    }

    fn evm_env(&self, header: &Header) -> Result<EvmEnv<SpecId>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnv<SpecId>, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<Block>,
    ) -> Result<EthBlockExecutionCtx<'a>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<EthBlockExecutionCtx<'_>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }
}

impl ConfigureEngineEvm<ExecutionData> for SovaEvmConfig {
    fn evm_env_for_payload(&self, payload: &ExecutionData) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env_for_payload(payload)
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a ExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_payload(payload)
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        self.inner.tx_iterator_for_payload(payload)
    }
}

/// reth's Ethereum block executor plus the SIP-7 pre-block record.
pub struct SovaBlockExecutor<'a, E> {
    inner: EthBlockExecutor<'a, E, &'a Arc<ChainSpec>, &'a RethReceiptBuilder>,
}

impl<E> BlockExecutor for SovaBlockExecutor<'_, E>
where
    E: Evm<DB: StateDB, Spec: Into<SpecId> + Clone, Tx = TxEnv>,
{
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = E;
    type Result = EthTxResult<E::HaltReason, TxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()?;
        if sip7_active() {
            record_zcash_block(self.inner.evm_mut())?;
        }
        Ok(())
    }

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        self.inner.execute_transaction_without_commit(tx)
    }

    fn commit_transaction(&mut self, output: Self::Result) -> GasOutput {
        self.inner.commit_transaction(output)
    }

    fn finish(self) -> Result<(Self::Evm, BlockExecutionResult<Receipt>), BlockExecutionError> {
        self.inner.finish()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }
}

fn refuse(why: String) -> BlockExecutionError {
    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
        format!("sova zcash-blocks: {why}").into(),
    ))
}

/// The SIP-7 §4.1 system call for the block `evm` is executing.
fn record_zcash_block<E>(evm: &mut E) -> Result<(), BlockExecutionError>
where
    E: Evm<DB: StateDB, Tx = TxEnv>,
{
    let number = evm.block().number();
    if number.is_zero() {
        return Ok(());
    }
    let Some(source) = zcash_source() else {
        return Err(refuse(format!(
            "no zcash index installed (sova block {number})"
        )));
    };
    let Some(e) = anchored_height(number, source.epoch_base()) else {
        return Err(refuse(format!(
            "anchored height out of range (sova block {number})"
        )));
    };
    let (Some((hash, time)), Some(summary)) = (source.block(e), source.block_summary(e)) else {
        return Err(refuse(format!(
            "no indexed record for zcash block {e} (sova block {number})"
        )));
    };
    let calldata = encode_zcash_blocks_record(e, hash, time, &summary);
    let beneficiary = evm.block().beneficiary();
    let res = evm
        .transact_system_call(SYSTEM_ADDRESS, ZCASH_BLOCKS, calldata)
        .map_err(|err| refuse(format!("system call failed: {err}")))?;
    if !res.result.is_success() {
        return Err(refuse(format!(
            "record for zcash block {e} reverted (sova block {number}): {:?}",
            res.result
        )));
    }
    let mut state = res.state;
    state.remove(&SYSTEM_ADDRESS);
    state.remove(&beneficiary);
    evm.db_mut().commit(state);
    Ok(())
}
