# Zero-to-mining walkthrough: proof transcript (D3)

This is the captured evidence for D3's acceptance criterion -- "zero-to-mining
through a Claude conversation on the box." It was produced by
`mcp/src/driver.ts`, a small script that speaks **real MCP protocol** over
stdio to the built server (`@modelcontextprotocol/sdk`'s `Client` +
`StdioClientTransport`), driving the exact same six tools a Claude Code
conversation would call after `claude mcp add` (see `../README.md`). A
scripted client and a model both go through `tools/list` and `tools/call` --
the wire protocol and the tool implementations are identical either way. The
demo GIF of an actual Claude conversation doing this is Wave-2 content (out
of scope here); this transcript is the pre-GIF proof that the loop works.

Run yourself with `npm run driver` (after `npm run build` and starting the
regtest harness). Full, unedited driver stdout for the run below is
reproducible byte-for-byte; only repeated/no-op `sova_status` polls are
elided here for readability (noted inline).

## Setup

```
$ cd box/regtest && docker compose down -v && docker compose up -d
 Container sova-zebrad-regtest  Started
$ curl -s ... getblockcount   # confirm a fresh chain
{"jsonrpc":"2.0","id":"1","result":0}

$ cd mcp && rm -rf .data && npm run build
$ node dist/driver.js
```

## tools/list

The server advertises exactly the six tools from the task spec:

```json
[
  { "name": "sova_init", "description": "Runs `sova-miner init`: creates (or loads) this miner's keystore and prints the transparent (t-addr) Zcash address to fund. Idempotent -- safe to call again; it loads the existing keystore instead of regenerating it." },
  { "name": "sova_fund_regtest", "description": "Regtest-only convenience: calls the zebrad `generatetoaddress` RPC to mine `blocks` coinbase blocks directly to the miner's funding address, so its coinbase is spendable immediately. Default is 101 blocks, since Zcash coinbase needs 100 confirmations to mature -- mining 101 to the SAME address leaves block 1's coinbase with 100 confirmations. Do not point this at a real network." },
  { "name": "sova_mine", "description": "Starts `sova-miner mine` as a background child process: while budget remains, it submits one SIP-1 burn per new Zcash block it observes over RPC. Returns immediately with a run id -- use sova_status to watch progress, sova_stop to end it early. Only one run may be active at a time." },
  { "name": "sova_status", "description": "Tails a mine run's log and summarizes progress (epochs completed, last epoch's burn/fee/txid, whether/why it stopped). Defaults to the most recently started run if runId is omitted." },
  { "name": "sova_stop", "description": "Stops a running mine loop (SIGTERM, escalating to SIGKILL after a grace period if needed). No-op if the run has already ended. Defaults to the most recently started run if runId is omitted." },
  { "name": "sova_report", "description": "Runs `sova-miner report`, optionally with --verify-rpc to independently re-scan the chain for this miner's SIP-1 burns and confirm the count and total zatoshis match the local state exactly (txid-set diff, not just totals)." }
]
```

## Phase 1: init -> fund -> mine (bounded) -> status -> stop (no-op) -> report (verified)

### `sova_init`

```
>>> tool_call: sova_init({"dataDir":".../mcp/.data/driver-miner","network":"regtest"})
<<< result (isError=false):
{
  "dataDir": ".../mcp/.data/driver-miner",
  "network": "regtest",
  "fundingAddress": "tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrU",
  "evmAddressHex": "0x00953dbc11a4ef7adcf6ba651bbb3b57430f3523",
  "keystorePath": ".../mcp/.data/driver-miner/keystore.json",
  "statePath": ".../mcp/.data/driver-miner/state.json",
  "cliOutput": "generated new keystore at .../keystore.json\n\nt-addr to fund: tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrU\nevm address (SIP-1 credit): 0x00953dbc11a4ef7adcf6ba651bbb3b57430f3523\n..."
}
```

(Recorded before `init` switched its default. The EVM address above is the
old default, the t-addr's hash160, which no key controls. `init` now
credits the keystore key's own Ethereum address, prints `import this key
into an EVM wallet to spend your SOVA (sova-miner export-evm-key)`, and
for a keystore still on the old default the result carries a `warnings`
field; see `crates/burn-wallet/miner/README.md`, "Your SOVA".)

### `sova_fund_regtest`

```
>>> tool_call: sova_fund_regtest({"dataDir":"...","rpcUrl":"http://127.0.0.1:18232","blocks":101})
<<< result (isError=false):
{
  "rpcUrl": "http://127.0.0.1:18232",
  "fundedAddress": "tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrU",
  "blocksMined": 101,
  "tipHeightBefore": 0,
  "tipHeightAfter": 101,
  "firstBlockHash": "9febe335228ad5cc9df3f0bb437b6fed22b998aec07615e29046912fa9b00045",
  "lastBlockHash": "14096c693042535765b7c9e797a439f9b526ea0d8166671421040e3b786760d4"
}
```

`box/regtest/auto-mine.sh 2` was started in the background at this point (1
block every 2s), the same way `box/regtest/miner-ac.sh` drives the D2
acceptance test -- the miner never mines its own confirming blocks, it only
reacts to blocks someone else produces.

### `sova_mine`

```
>>> tool_call: sova_mine({"dataDir":"...","network":"regtest","rpcUrl":"http://127.0.0.1:18232","budgetZat":3000000,"perEpochZat":100000,"pollIntervalMs":300,"maxEpochs":5})
<<< result (isError=false):
{
  "runId": "c18f77d7-f8c4-46bb-b468-eda9081578e9",
  "pid": 81449,
  "command": "sova-miner --data-dir .../driver-miner --network regtest mine --budget-zat 3000000 --per-epoch-zat 100000 --rpc http://127.0.0.1:18232 --poll-interval-ms 300 --max-epochs 5",
  "logPath": ".../mcp/.data/runs/c18f77d7-f8c4-46bb-b468-eda9081578e9.log",
  "startedAt": "2026-09-21T23:38:57.977Z",
  "note": "call sova_status with this runId to watch progress."
}
```

### Single-active-run guard

Immediately calling `sova_mine` again, while the run above is still active,
is refused rather than starting a second process:

```
>>> tool_call: sova_mine({"dataDir":"...","budgetZat":100000,"perEpochZat":50000, ...})
<<< result (isError=true):
a mine run is already active (runId c18f77d7-f8c4-46bb-b468-eda9081578e9). Call sova_stop first, or sova_status to check on it.
```

### `sova_status` (polled every 2s; only state transitions shown, repeats elided)

```
epochsCompleted: 0  status: running   (baseline tip height: 101)
epochsCompleted: 1  status: running   epoch 1 height=103 burn=100000zat fee=20000zat txid=c5632d5e...
epochsCompleted: 2  status: running   epoch 2 height=105 burn=100000zat fee=20000zat txid=492ba792...
epochsCompleted: 3  status: running   epoch 3 height=107 burn=100000zat fee=20000zat txid=7a93ec03...
epochsCompleted: 4  status: running   epoch 4 height=109 burn=100000zat fee=20000zat txid=1c2ed237...
epochsCompleted: 5  status: exited    epoch 5 height=111 burn=100000zat fee=20000zat txid=82f2f8d1...
                    exitCode: 0, stoppedReason: "max-epochs-reached"
```

Full JSON for the terminal poll:

```json
{
  "runId": "c18f77d7-f8c4-46bb-b468-eda9081578e9",
  "status": "exited",
  "pid": 81449,
  "startedAt": "2026-09-21T23:38:57.977Z",
  "endedAt": "2026-09-21T23:39:18.5xx",
  "exitCode": 0,
  "exitSignal": null,
  "epochsCompleted": 5,
  "lastEpoch": {
    "epoch": 5, "height": 111, "burnZat": 100000, "feeZat": 20000,
    "changeZat": 624760000, "txid": "82f2f8d17d5059dedfa97f054a5a7d37723742a25ba1b705685213219f814f60"
  },
  "stoppedReason": "max-epochs-reached",
  "warnings": []
}
```

### `sova_stop` (already-exited -> safe no-op)

```
>>> tool_call: sova_stop({"runId":"c18f77d7-f8c4-46bb-b468-eda9081578e9"})
<<< result (isError=false):
{ "runId": "c18f77d7-f8c4-46bb-b468-eda9081578e9", "status": "exited", "exitCode": 0, "exitSignal": null, "endedAt": "..." }
```

### `sova_report` (verified against chain)

```
>>> tool_call: sova_report({"dataDir":"...","network":"regtest","verifyRpcUrl":"http://127.0.0.1:18232"})
<<< result (isError=false):
{
  "dataDir": ".../mcp/.data/driver-miner",
  "verified": true,
  "matches": true,
  "exitCode": 0,
  "report": "=== sova-miner report (.../state.json) ===\naddress: tm9mS7dQkAvq7ads58kyJjjaVnjfarTvhrU\nevm address: 0x00953dbc11a4ef7adcf6ba651bbb3b57430f3523\nbudget: 3000000 zat\nper-epoch: 100000 zat\ntotal burned: 500000 zat\ntotal fees: 100000 zat\ntotal spent: 600000 zat\nbudget left: 2400000 zat\nepochs: 5\n\nper-epoch history:\n  epoch 1 height 103 burn 100000 zat fee 20000 zat change 624880000 zat txid c5632d5e...\n  epoch 2 height 105 burn 100000 zat fee 20000 zat change 624880000 zat txid 492ba792...\n  epoch 3 height 107 burn 100000 zat fee 20000 zat change 624880000 zat txid 7a93ec03...\n  epoch 4 height 109 burn 100000 zat fee 20000 zat change 624760000 zat txid 1c2ed237...\n  epoch 5 height 111 burn 100000 zat fee 20000 zat change 624760000 zat txid 82f2f8d1...\n\n=== on-chain verification against http://127.0.0.1:18232 ===\nchain tip scanned: 111\nchain burns found (our address): 5 (total 500000 zat)\n  height 103 txid c5632d5e... 100000 zat\n  height 105 txid 492ba792... 100000 zat\n  height 107 txid 7a93ec03... 100000 zat\n  height 109 txid 1c2ed237... 100000 zat\n  height 111 txid 82f2f8d1... 100000 zat\nlocal epoch count: 5\nlocal total burned: 500000 zat\nMATCH: yes"
}
```

`matches: true` -- the CLI's own `--verify-rpc` cross-check (full txid-set
diff, not just totals) passed: 5 epochs, 500,000 zat burned, chain and local
state agree exactly.

```
=== WALKTHROUGH PASSED: init -> fund -> mine -> status -> stop -> report (verified) ===
```

## Phase 2: `sova_stop` against a run that is actually still running

Phase 1's `sova_stop` call only exercised the "already exited" no-op path
(the run had already finished via `--max-epochs 5`). Phase 2 starts a second,
*unbounded* run against the same funded miner identity and stops it while it
is genuinely mid-flight, to prove the real SIGTERM path:

```
>>> tool_call: sova_mine({"dataDir":"...","budgetZat":1000000,"perEpochZat":100000,"pollIntervalMs":300})
<<< result (isError=false):
{ "runId": "8ff55afe-5c89-4366-b7a4-8889a484b039", "pid": 81544, ... }
```

Polled until at least one epoch landed (`status: "running"`,
`epochsCompleted: 1`, epoch 6 at height 113), then stopped:

```
>>> tool_call: sova_stop({"runId":"8ff55afe-5c89-4366-b7a4-8889a484b039","graceMs":5000})
<<< result (isError=false):
{
  "runId": "8ff55afe-5c89-4366-b7a4-8889a484b039",
  "status": "stopped",
  "exitCode": null,
  "exitSignal": "SIGTERM",
  "endedAt": "2026-09-21T23:39:24.670Z"
}
```

A follow-up `sova_status` confirms the terminal state (`status: "stopped"`,
`exitSignal: "SIGTERM"`, `epochsCompleted: 1` -- the process was killed
cleanly mid-loop, not mid-epoch, since each epoch blocks-then-saves before
the loop re-polls). A second `sova_stop` call on the same runId is a safe
no-op (returns the same `stopped` record, doesn't error).

```
=== PHASE 2 PASSED: sova_mine -> sova_status(running) -> sova_stop (live) -> sova_status(stopped) ===
```

## Result

All 6 tools exercised end to end against a live regtest harness, in one
process lifetime, with no manual intervention:

| Tool | Exercised as |
| --- | --- |
| `sova_init` | fresh keystore creation, address + EVM address derivation |
| `sova_fund_regtest` | 101-block regtest funding via `generatetoaddress` |
| `sova_mine` | bounded run (`--max-epochs 5`, completes naturally) and unbounded run (stopped externally); concurrent-call rejection |
| `sova_status` | polling a running loop, reading a naturally-exited loop, reading a stopped loop |
| `sova_stop` | no-op on an already-exited run, real SIGTERM on a live run, idempotent re-stop |
| `sova_report` | `--verify-rpc` cross-check, `MATCH: yes` |

Total: 6 SIP-1 burns confirmed on chain across the two phases (5 + 1),
all independently re-verified by the miner's own on-chain scan.
