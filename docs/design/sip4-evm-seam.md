# SIP-4 EVM seam: research notes and spike

Status: research spike on `research/evm-seam` (2026-09-23). Everything
here is checked against the exact sources this workspace builds:
reth `v2.6.0` (git `73a3a00`), alloy-evm `0.39.0`, revm `43.0.x`
(`Cargo.lock`). Citation roots:

| Prefix | Path |
|---|---|
| `reth:` | `~/.cargo/git/checkouts/reth-e231042ee7db3fb7/73a3a00/crates/` |
| `alloy-evm:` | `~/.cargo/registry/src/index.crates.io-*/alloy-evm-0.39.0/src/` |
| `revm-handler:` etc. | `~/.cargo/registry/src/index.crates.io-*/revm-handler-43.0.2/src/` (same for `revm-precompile-43.0.2`, `revm-context-43.0.2`, `revm-context-interface-43.0.1`, `revm-interpreter-43.0.1`, `revm-primitives-43.0.0`) |

**Corrections to SIP-4 found here** (details below):

1. `PrecompileError::Fatal` does **not** halt the engine in v2.6.0. On the
   live-sync path the block is refused: `newPayload` gets an error, the
   block is **not** added to the invalid-header cache, and the engine
   keeps running. That still means no divergence, and it is better for
   liveness. But §5 and §6 should say "refuse", not "halt". (Q5)
2. **One path does mark the block invalid:** the backfill pipeline. An
   execution error there unwinds and inserts the block into the
   in-memory invalid-header cache. (Q5)
3. A naive `value > 0 → revert` breaks DELEGATECALL. `PrecompileInput::value`
   is the *apparent* value for DELEGATECALL. Revert only on a *direct*
   call with value. A mutation test in the spike shows the difference. (Q6)
4. `examples/precompile-cache` does not exist at this revision. Only
   `examples/custom-evm` does, and it matches. (Q1)

## Q1. Wiring: `SovaExecutorBuilder` → `EthEvmConfig<ChainSpec, SovaEvmFactory>`

- Template: `reth: ../examples/custom-evm/src/main.rs:46-103` (`MyEvmFactory` plus
  `MyExecutorBuilder` returning `EthEvmConfig<ChainSpec, MyEvmFactory>`). It
  matches v2.6.0. `examples/precompile-cache` is **absent** from this
  checkout.
- Constructor: `EthEvmConfig::new_with_evm_factory(chain_spec, factory)`,
  `reth: ethereum/evm/src/lib.rs:115`.
- Bounds for `EthEvmConfig: ConfigureEvm` (`reth: ethereum/evm/src/lib.rs:139-156`):
  `EvmF: EvmFactory<Tx: TransactionEnvMut + FromRecoveredTx<TransactionSigned> + FromTxWithEncoded<TransactionSigned>, Spec = SpecId, BlockEnv = BlockEnv, Precompiles = PrecompilesMap> + Clone + Debug + Send + Sync + Unpin + 'static`.
- `EvmFactory` trait: `alloy-evm: evm.rs:259-307`. The stock
  `EthEvmFactory` is at `alloy-evm: eth/mod.rs:265-292`.
- Precise v2.6.0 types (as in the spike, `crates/evm/src/zcash.rs`):

```rust
impl EvmFactory for SovaEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>>> = EthEvm<DB, I, PrecompilesMap>;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Tx = TxEnv;
    type Error<DBError: DBErrorMarker> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;
    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let mut evm = EthEvmFactory::default().create_evm(db, input);
        install_zcash_query(evm.precompiles_mut(), self.resolve_source()); // extend_precompiles
        evm
    }
    // create_evm_with_inspector: same, via EthEvmFactory::create_evm_with_inspector
}
```

  Every type is reachable through the `reth-ethereum` facade:
  `reth_ethereum::evm::primitives` is `reth_evm`, which glob-re-exports
  alloy-evm (`reth: evm/evm/src/lib.rs:57-60`), and
  `reth_ethereum::evm::revm` is `reth_revm`, which re-exports revm.
  **No new crate dependencies** were needed. Note that
  `reth_ethereum::evm::revm::database` is *reth's* database module, which
  shadows revm's; use `revm::db::{CacheDB, EmptyDB}`.
- `PrecompilesMap::extend_precompiles` (`alloy-evm: precompiles.rs:235-242`)
  goes through `apply_precompile`, which converts the map to dynamic form
  and inserts both the precompile **and its address into the warm set**
  (`:157-178`, `:171`). That matters for Q4.
- **Differences from the stock builder.** reth's `EthereumExecutorBuilder`
  (`reth: ethereum/node/src/node.rs:617-656`) returns
  `EthEvmConfig<_, RethEvmFactory>` and attaches the sender-recovery
  cache. Without the `jit` cargo feature (not enabled here: no `revmc` in
  `Cargo.lock`), `RethEvmFactory` is a newtype over `EthEvmFactory`
  (`reth: ethereum/evm/src/factory.rs:27-50`). So wrapping `EthEvmFactory`
  changes no behaviour. The spike copies over
  `with_sender_recovery_cache` and bails on `--jit`.
- `SovaNode` already selects `SovaExecutorBuilder`
  (`crates/engine/src/node.rs:59,68`). Nothing else in `crates/engine`
  assumes the stock EVM type. The payload builder is generic over
  `Evm: ConfigureEvm` (`crates/engine/src/builder.rs`), and workspace
  clippy is clean.

## Q2. Block number: yes, on every path

- `PrecompilesMap::run` builds `EvmInternals::new(journaled_state, block, cfg, tx)`
  from the live context on **each call** (`alloy-evm: precompiles.rs:564-595`,
  `:587`). `input.internals().block_number()` returns `U256`
  (`alloy-evm: traits.rs:519`). The spike reads it per call. The test
  `anchor_answer_follows_block_number` changes `ctx.block.number` on a
  single EVM and gets the new answer.
- **Import (engine):** `evm_config.evm_with_env(&mut db, env.evm_env)`
  (`reth: engine/tree/src/tree/payload_validator.rs:1037-1046`). The env
  comes from the payload or header. For a header,
  `EthEvmConfig::evm_env` → `EvmEnv::for_eth_block(header, ..)`
  (`reth: ethereum/evm/src/lib.rs:209-216`). For a payload,
  `block_env.number = payload block_number` (`:318-319`).
- **Payload building:** `next_evm_env(parent, attrs)` → `for_eth_next_block`
  (`reth: ethereum/evm/src/lib.rs:218-232`), so `N = parent + 1`.
- **Backfill pipeline:** same `ConfigureEvm`, per block
  (`reth: stages/stages/src/stages/execution/mod.rs:359`).
- **RPC `eth_call` / `eth_estimateGas`:** `evm_env_at(block_id)`
  (`reth: rpc/rpc-eth-api/src/helpers/state.rs:393-418`). A tagged or
  numbered block uses that header's number.
  - **`pending`** (`reth: rpc/rpc-eth-api/src/helpers/pending_block.rs:68-102`):
    if a local pending block exists, its header number is used.
    Otherwise it is `next_evm_env(latest)`, so `N = latest + 1` and
    `E_N = latest + B`. If the follower has not scanned that height,
    the call errors (Q5). That is correct.
  - **Block overrides:** `eth_call` block overrides can set `number`
    (`alloy-evm: overrides.rs:91-92`). A caller can choose `N` and
    therefore `E_N`. This affects RPC only. It lets callers probe any
    anchored height, and a miss is an RPC error.
- **What the precompile cannot see:** the header's
  `parent_beacon_block_root` (the committed anchor) is not in `BlockEnv`.
  The block executor applies it only through the EIP-4788 system call
  (`alloy-evm: eth/block.rs:185`). The testnet genesis alloc is cleared
  (`bin/sova/src/chain.rs:151`), so the beacon-roots contract does not
  exist there. See "Still needed", item 3.

## Q3. Caching

- `DynPrecompile::new_stateful` wraps the closure in `StatefulPrecompile`,
  whose `supports_caching()` returns `false`
  (`alloy-evm: precompiles.rs:632-639`, `:927-941`). Plain
  `DynPrecompile::new` would return **true** for any id except
  `Identity` (`:24-31`, `:806-821`). Using `new_stateful` is therefore
  required, not optional.
- The engine wraps only cacheable precompiles, in both places it wraps
  any:
  - block execution: `map_cacheable_precompiles(..CachedPrecompile::wrap..)`
    (`reth: engine/tree/src/tree/payload_validator.rs:1049-1066`)
  - prewarming: the same call (`reth: engine/tree/src/tree/payload_processor/prewarm.rs:642-651`)
  - filter: `map_cacheable_precompiles` → `map_precompiles_filtered(.., |_, p| p.supports_caching())`
    (`alloy-evm: precompiles.rs:84-89`)
  - The cache is keyed on calldata plus spec **across blocks**
    (`reth: engine/tree/src/tree/precompile_cache.rs:183-191`). It is
    process-wide (`PrecompileCacheMap`, `:20-44`). The kill switch is
    `--engine.disable-precompile-cache` (`reth: node/core/src/args/engine.rs:43`).
- **Other caches:** none for precompile outputs.
  - The payload builder's `CachedReads` caches state-DB reads only
    (`reth: ethereum/payload/src/lib.rs:162,186`).
  - RPC `eth_call`, `eth_estimateGas` and `eth_simulateV1` create fresh
    EVMs with no precompile cache. `eth_simulateV1` state overrides can
    *move* precompiles (`apply_precompile_overrides`,
    `reth: rpc/rpc-eth-api/src/helpers/call.rs:233-262`), including
    0x5A00. That is RPC-only and harmless.
- Spike test `precompile_is_not_cacheable`: `supports_caching() == false`
  on the registered precompile. It also runs the engine's exact
  `map_cacheable_precompiles` call and checks that 0x5A00 is not visited
  while 0x01 is.

## Q4. Warm address: yes, 100 gas

- At tx start, `pre_execution::load_accounts` calls
  `journal.warm_precompiles(precompiles.warm_addresses())`
  (`revm-handler: pre_execution.rs:25-40`). `PrecompilesMap::warm_addresses`
  returns the dynamic `addresses` set (`alloy-evm: precompiles.rs:597-602`),
  and `apply_precompile` inserts into that set (`:171`).
- 0x5A00 is 23,040, which exceeds `SHORT_ADDRESS_CAP` = 300
  (`revm-primitives: lib.rs:61-77`). It is therefore not in the bitvec
  fast path, and `is_warm` falls through to `precompile_set.contains`
  (`revm-context: journal/warm_addresses.rs:68-83`, `:130-139`).
- **Caveats.**
  - The warm set is reloaded only when `set_spec` reports a change or the
    journal's set is empty (`pre_execution.rs:31-40`).
    `PrecompilesMap::set_spec` always returns `false`
    (`precompiles.rs:560-562`). Precompiles must therefore be installed
    before the first tx, which `create_evm` does.
  - Do **not** use `set_precompile_lookup`. Addresses resolved through a
    lookup are always cold (`precompiles.rs:374-376`).
- Spike test `precompile_address_is_warm`: a STATICCALL to 0x5A00 uses
  exactly 2,300 gas less than a STATICCALL to an empty address:
  (100 warm + 200 anchor) − 2,600 cold.

## Q5. Fatal errors

**Precompile to EVM:** return `Err(PrecompileError::Fatal(String))` (or
`FatalAny`), from `revm-precompile: interface.rs:592-605`. In revm 43,
`Err` means fatal *only*. Revert, halt and OOG are
`Ok(PrecompileOutput { status, .. })` (`interface.rs:23-32`, `:53-62`).
The path from there:

1. `PrecompilesMap::run` does `.map_err(|e| e.to_string())?`
   (`alloy-evm: precompiles.rs:592`).
2. `make_call_frame` does `precompiles.run(..).map_err(ERROR::from_string)?`
   (`revm-handler: frame.rs:207`), which becomes `EVMError::Custom("fatal: …")`
   (`revm-context-interface: result.rs:756-759`). The transaction aborts.
   Nothing is reverted or committed.

**EVM to block executor:** the Ethereum block executor maps a transact
error with `BlockExecutionError::evm(err, tx_hash)`
(`alloy-evm: eth/block.rs:238-240`). A non-invalid-tx error with
`is_fatal()` goes to `BlockExecutionError::Internal(InternalBlockExecutionError::EVM{..})`
(`alloy-evm: block/error.rs:182-196`), and `EVMError::Custom` is fatal
(`alloy-evm: error.rs:101-107`). The spike test `missing_anchor_is_fatal`
asserts the whole chain up to `BlockExecutionError::Internal`.

**Engine, verified.** SIP-4 §5 cites
`reth: engine/primitives/src/error.rs:109-132` correctly:
`ensure_validation_error` maps `Execution(Internal)` →
`InsertBlockProcessingError::Fatal(InsertBlockFatalError::BlockExecutionError)`.
Its consequence is different from what §5 claims:

- **`engine_newPayload`:** `on_insert_block_error` returns early through
  `?` **before** the invalid-header insert
  (`reth: engine/tree/src/tree/mod.rs:3283-3291`, the insert is at
  `:3318`). `on_new_payload`'s error is **sent back to the caller**
  (`mod.rs:1693-1716`) as `BeaconOnNewPayloadError::Internal`
  (`engine/primitives/src/error.rs:39-46`). The loop only exits on an
  `Err` from `on_engine_message` (`mod.rs:586-592`), and this path does
  not produce one. **Net effect:** the block is refused, stays retryable,
  is not cached as invalid, and the engine keeps running.
- **Downloaded or buffered blocks:** `warn!` and continue
  (`mod.rs:2697-2700`, `:3077-3080`). Same outcome.
- **Backfill pipeline (the exception):**
  `executor.execute_one(..).map_err(StageError::Block{ Execution })`
  (`reth: stages/stages/src/stages/execution/mod.rs:359-362`) →
  `ControlFlow::Unwind { bad_block }` (`reth: stages/api/src/pipeline/mod.rs:608-622`)
  → `self.state.invalid_headers.insert(bad_block)`
  (`reth: engine/tree/src/tree/mod.rs:1841-1844`). The cache is in
  memory, so a restart clears it. Still, it breaks SIP-4's "never marks
  invalid". Mitigation: the §1 pre-execution anchor check has to run on
  the pipeline too. The bodies downloader calls
  `validate_block_pre_execution` (`reth: net/downloaders/src/bodies/request.rs`),
  so SovaConsensus's transient hold should stop an unscanned block
  before execution. A Fatal at execution then only fires on a real
  index bug.
- **Payload builder:** a Fatal falls through to
  `Err(err) => return Err(PayloadBuilderError::evm(err))`
  (`reth: ethereum/payload/src/lib.rs:413-414`), and the build attempt
  fails. **Poison-tx risk:** if a pool tx can trigger Fatal, every build
  fails for as long as that tx stays in the pool. Rule for the full
  implementation: **Fatal only when `E_N` is above the index watermark,
  or the index hash at `E_N` disagrees with the committed anchor.** A
  well-indexed miss (unknown txid, missing vout) is a status result
  (`NOT_FOUND` / `NO_SUCH_OUTPUT`) and never Fatal. The sealer only
  builds epochs its follower has emitted, so `anchor()` cannot miss
  during building.

**`eth_call` / `eth_estimateGas`:** `EVMError::Custom(s)` becomes
`EthApiError::EvmCustom(s)` (`reth: rpc/rpc-eth-types/src/error/mod.rs:590`),
displayed as `"Revm error: {s}"` (`:183-184`), then
`internal_rpc_err` (`:315`), which is JSON-RPC code `-32603`. The user
sees:
`{"code":-32603,"message":"Revm error: fatal: sova zcash precompile: zcash index has no block at anchored height 4383011 (sova block 12)"}`.
`eth_estimateGas` uses the same `from_evm_err` mapping
(`reth: rpc/rpc-eth-api/src/helpers/estimate.rs:158-275`).

## Q6. Gas, ABI, value, call kinds

- **Output.** `PrecompileOutput { status, gas_used, gas_refunded, state_gas_used, state_gas_spilled, reservoir, bytes }`
  (`revm-precompile: interface.rs:110-131`). Constructors: `::new(gas, bytes, reservoir)`,
  `::revert(gas, bytes, reservoir)`, `::halt(reason, reservoir)`
  (`:141-178`). Pass `input.reservoir` through unchanged. The engine
  cache treats a changed reservoir as a sign the precompile is stateful
  (`reth: engine/tree/src/tree/precompile_cache.rs:~200-211`).
- **Conversion** (`revm-handler: precompile_provider.rs:95-125`):
  - `gas_used > gas_limit` → `PrecompileOOG`, even if the status is Success.
  - Success → `Return`. Revert → `Revert` (output bytes returned,
    unused gas refunded to the caller).
  - `Halt(OOG)` → `PrecompileOOG`. Any other halt → `PrecompileError`.
    A halt spends all gas forwarded to the call.
  - The spike returns `halt(OutOfGas)` when `input.gas < 200`.
- **Value.** The transfer to the precompile happens **before** it runs
  (`revm-handler: frame.rs:183-194`). A non-ok result reverts the
  checkpoint and so undoes the transfer (`frame.rs:207-216`). A revert
  therefore strands nothing.
  - `PrecompileInput::value` is `inputs.call_value()`
    (`alloy-evm: precompiles.rs:585`), which is `CallValue::get()`
    (`revm-interpreter: interpreter_action/call_inputs.rs:222-224`,
    `:266-289`). For DELEGATECALL that is the parent frame's
    **apparent** value.
  - The correct rule is `input.is_direct_call() && value > 0 → revert`,
    using `is_direct_call` (`target == bytecode`,
    `alloy-evm: precompiles.rs:736-738`). CALLCODE with value is a
    self-transfer and not direct, so it is allowed.
  - The spike's contract tests cover STATICCALL, DELEGATECALL from a
    frame holding `msg.value = 1` (succeeds), and CALL with value 1
    (reverts). Dropping `is_direct_call()` makes the DELEGATECALL test
    fail; this was checked by mutation.
- **STATICCALL:** `input.is_static` is set. A read-only precompile needs
  nothing more.
- **Malformed calldata:** revert, as the SIP says. The spike requires
  `data == selector` exactly for `anchor()`. That is a spec choice:
  Solidity-generated calls never append data. Revert and OOG are
  decided **before** the index lookup, so a miss can only make fatal a
  call that would otherwise have succeeded.

## Q7. Where the index handle comes from

- **Factory bounds:** `Clone + Debug + Send + Sync + Unpin + 'static`
  (Q1). `Debug` has to be written by hand when the factory holds a
  trait object.
- **Closure bounds:** `DynPrecompile::new_stateful` needs only
  `F: Fn(PrecompileInput<'_>) -> PrecompileResult + 'static`, with no
  `Send`/`Sync` (`alloy-evm: precompiles.rs:634-637`). The map is built
  per EVM inside `create_evm`, so the closure only has to capture a
  clone of the factory's `Arc<dyn AnchorSource>`. The trait is
  `Send + Sync + Debug + 'static`.
- **Spike wiring:**
  - `evm::zcash::set_anchor_source(Arc<dyn AnchorSource>)` is a
    `OnceLock` global (the `expectations::global()` pattern,
    `crates/engine/src/expectations.rs:202-205`).
  - `SovaEvmFactory::default()` resolves the global once per
    `create_evm*`, which happens per block and per RPC call.
    `SovaEvmFactory::with_anchor_source` injects a source directly, for
    tests.
  - `crates/engine/src/anchor.rs::ExpectationsAnchor` implements
    `AnchorSource` over the C5 expectations map. Record `N = E − B + 1`
    already holds the scanned epoch hash, so the spike needs no new
    store.
  - `bin/sova/src/main.rs` calls `engine::anchor::install(base_height)`.
    It runs after launch because `SOVA_EPOCH_BASE` is parsed there, which
    leaves a startup window where calls are refused. The real version
    should install before launch.
- The `evm` crate cannot depend on `engine`, because `engine` depends on
  `evm`. The trait therefore lives in `evm` and `engine` implements it.

## Q8. Storage for `ZcashIndex`

**Recommendation for testnet: in memory. Use a `BTreeMap` behind an
`RwLock`, fed by the same follower loop as C5 and rebuilt by rescanning
from `B` at every start.** Reasons:

- It is already how C5 works. `run_expectations` builds
  `Follower::new(base_height, REORG_WINDOW)` on every start
  (`crates/engine/src/expectations.rs:231-237`). The follower already
  fetches `getblock` plus a `getrawtransaction` for every tx of every
  epoch (`crates/consensus/src/zebrad.rs:86-120`). Keeping those records
  in memory adds no RPC cost and no new failure mode.
- It is consistent by construction. After a restart the index equals
  f(zebrad's canonical chain), the same function C5 computes. There is
  no second on-disk store to keep crash-consistent with reth's MDBX, and
  none to unwind in step with it. Blocks whose `E_N` has not been
  rescanned yet simply **hold** (SIP-4 §1 transient), then import.
- reth never re-executes old blocks on restart. It resumes from its
  persisted head. So the only consumers of historical `E_N` are
  RPC-at-old-blocks and a fresh node's sync, and a fresh node rescans
  from `B` anyway.
- **Cost.** Memory grows with transactions since `B`: txid, height,
  index, version, and transparent outputs with scripts. A rough figure
  at ~20 tx/block and 1,152 blocks/day is ~23k records/day, a few MB per
  day. That is fine for testnet. Startup cost is the rescan, which C5
  already pays.
- **When to persist** (mainnet, or once rescans get slow): use a
  pure-Rust embedded store such as **redb** (single file, ACID), holding
  index data plus a `(height, hash)` watermark, and verify it against
  zebrad on open by re-walking the follower window. A custom MDBX table
  inside reth's DB would tie Zcash unwinds to reth's unwind machinery.
  Avoid it unless the index must be atomic with Sova state, which it
  does not need to be. Put the store behind the same trait so the
  in-memory version can be swapped out.

## What the spike implements

| File | What |
|---|---|
| `crates/evm/src/zcash.rs` | `AnchorSource`, the global, `SovaEvmFactory`, the `anchor()` precompile, and 9 tests |
| `crates/evm/src/lib.rs` | `SovaExecutorBuilder` → `EthEvmConfig<ChainSpec, SovaEvmFactory>` (plus sender-recovery cache and a `--jit` bail) |
| `crates/engine/src/anchor.rs` | `ExpectationsAnchor` (an `AnchorSource` over C5 expectations), `install(base)`, and a test |
| `bin/sova/src/main.rs` | `engine::anchor::install(base_height)` |

`anchor()` behaviour, in order:

1. A direct call with value reverts.
2. Selector not equal to `d3fb73b4` reverts.
3. Gas below 200 halts out-of-gas.
4. Otherwise it computes `E_N = N + B − 1`, using checked arithmetic;
   overflow is Fatal.
5. If the source has no hash at `E_N`, the result is Fatal.
6. Otherwise it returns `abi.encode(uint64 E_N, bytes32 hash)` and
   charges 200 gas.

Tests (all passing; `cargo clippy --workspace --all-targets -D warnings` is clean):

- `anchor_answer_follows_block_number` (a): blocks 10 and 11 give
  different bytes, both on fresh EVMs and on one EVM whose block number
  is changed between calls.
- `precompile_is_not_cacheable` (b): `supports_caching() == false`, and
  the engine's `map_cacheable_precompiles` skips 0x5A00.
- `missing_anchor_is_fatal` (c): `EVMError::Custom("fatal: …")` becomes
  `BlockExecutionError::Internal`. Covers both an unknown height and an
  empty source.
- `value_malformed_and_oog_do_not_touch_the_index` (d): value > 0
  reverts, bad calldata reverts, and low gas halts OOG, all against an
  empty source.
- `staticcall_and_delegatecall_work_value_call_reverts`: hand-assembled
  caller contract.
- `precompile_address_is_warm`: gas delta of exactly −2,300.
- `selector_is_keccak_of_signature`, `anchored_height_formula`, and
  `engine::anchor::tests::answers_from_expectations_by_zcash_height`.

## Still needed for the full implementation

1. **Real `ZcashIndex`.** It needs txid → (height, index, version,
   outputs), height → (hash, time), and later spent outpoints. It is fed
   by the follower and unwound on `Rollback`. Every query must filter
   **`record.height ≤ E_N`**, because the index runs ahead of `E_N`. A
   tx mined above `E_N` must answer `NOT_FOUND` (or `NOT_YET` for
   heights), never `OK`. `spentBy` needs the same filter on the spender
   height. This is the main determinism trap.
2. **Fatal discipline.** Return Fatal only when `E_N` is above the
   watermark or the hash disagrees (Q5, poison-tx). Everything else is a
   status code.
3. **Anchor binding.** The precompile answers `anchor().hash` from the
   index. Equality with the header's `parent_beacon_block_root` holds at
   import only because of the §1 pre-execution check. After a Zcash
   reorg rolls the index back, `eth_call` and `debug_trace*` at old
   blocks will read the new branch. This is RPC-only. Two options:
   - Deploy the EIP-4788 beacon-roots contract in the reset genesis
     (today the alloc is cleared, `bin/sova/src/chain.rs:151`). The
     precompile can then `internals.sload` the committed root for
     `block.timestamp` and cross-check it.
   - Accept the RPC-only discrepancy and document it.
4. **Consensus side (§1).** `AnchorUnscanned` and `AnchorOffFork`
   transient errors in `SovaConsensus::validate_block_pre_execution`,
   plus `is_transient_error`. Confirm they fire on the bodies-downloader
   path so backfill never reaches a Fatal (Q5, pipeline exception).
5. **The remaining methods** (`blockAt`, `txInfo`, `txOutput`,
   `burnInfo`): the gas table, ABI golden vectors, and
   `txOutput`'s `4000 + 8·len` with the OOG check before the lookup.
6. **Activation gate.** Registering the precompile changes what calls to
   0x5A00 do from genesis. That is fine at the testnet reset. Otherwise,
   gate it in `create_evm` on `input.block_env.number` or timestamp.
7. **Install before launch** (move the `SOVA_EPOCH_BASE` parse ahead of
   `NodeBuilder::launch`).
8. **Docs fix in SIP-4 §5 and §6.** "Engine halts" should read "block
   refused (newPayload error, not cached invalid, engine keeps running);
   backfill caches it invalid in memory".
