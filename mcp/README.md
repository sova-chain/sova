# sova-miner MCP server (D3)

Wraps the `sova-miner` CLI ([D2](../crates/burn-wallet/miner)) in a Model
Context Protocol server, so a Claude agent can mine SOVA end to end just by
talking: *"tell your Claude to mine."* Six tools cover the whole loop --
create a keystore, fund it on regtest, run the budget-capped mine loop,
check on it, stop it, and pull a chain-verified report.

## Deviation from the plan (approved)

`docs/WORKPLAN.md`'s original sketch for D3 assumed a separate repo. This
implementation lives in-tree at `sova-chain/mcp/` instead -- a single-repo
layout is simpler to build, test, and keep in sync with the miner CLI it
wraps, and nothing about the MCP protocol requires a separate package
boundary. Approved as part of the D3 task brief.

## What's here

| Path | Purpose |
| --- | --- |
| `src/index.ts` | The MCP server: registers the 6 tools, stdio transport. |
| `src/runManager.ts` | Tracks the (at most one) active `mine` child process: pid, log path, status. |
| `src/minerCli.ts` | Runs `sova-miner init`/`report` as one-shot child processes. |
| `src/minerState.ts` | Reads `<data-dir>/state.json` (written by the CLI) for structured status. |
| `src/rpc.ts` | Minimal JSON-RPC client, used only by `sova_fund_regtest`. |
| `src/logParse.ts` | Parses the mine loop's stdout into a structured summary. |
| `src/driver.ts` | A scripted MCP client that exercises every tool end to end (see `docs/walkthrough.md`). |
| `docs/walkthrough.md` | Captured transcript proving the full loop against a live regtest harness. |
| `.data/` (gitignored) | Keystore, state sidecar, and per-run logs live here. |

## Tools

| Tool | Does | Key inputs |
| --- | --- | --- |
| `sova_init` | Runs `sova-miner init`; creates/loads the keystore, returns the funding address. | `dataDir?`, `network?` (default `regtest`), `evmAddress?` |
| `sova_fund_regtest` | Regtest-only: `generatetoaddress` N blocks straight to the miner's address (default 101 -- one mature coinbase). | `dataDir?`, `rpcUrl?`, `blocks?` (default 101), `address?` |
| `sova_mine` | Starts `sova-miner mine` as a background process; returns a `runId` immediately. Only one run at a time. | `budgetZat`, `perEpochZat`, `dataDir?`, `network?`, `rpcUrl?`, `pollIntervalMs?`, `maxEpochs?` |
| `sova_status` | Tails a run's log; returns epochs completed, last epoch's burn/fee/txid, and why it stopped (if it has). | `runId?` (defaults to most recent), `tailLines?` |
| `sova_stop` | SIGTERM (then SIGKILL after a grace period) a running mine loop. No-op if already ended. | `runId?`, `graceMs?` |
| `sova_report` | Runs `sova-miner report`, optionally `--verify-rpc` for a full on-chain cross-check. | `dataDir?`, `network?`, `verifyRpcUrl?` |

Full JSON schemas are in `src/index.ts` (zod, self-describing via
`tools/list`) -- ask Claude to list the server's tools once it's registered
and it will show the same descriptions.

## Prerequisites

- Node.js >= 18.17 (tested with Node 23 / npm 11).
- The `sova-miner` binary built:
  ```bash
  cd ../crates/burn-wallet && cargo build --release -p sova-miner
  ```
  (nested cargo workspace -- see that crate's `Cargo.toml` header comment
  for why it can never share a build graph with reth.)
- Docker, for the regtest harness (`../box/regtest`).

## Build

```bash
npm install
npm run build   # tsc -> dist/
npm start        # runs dist/index.js over stdio
```

## Zero-to-mining walkthrough

### 1. Start the harness

```bash
cd ../box/regtest
docker compose up -d
docker compose logs -f zebrad   # wait for "RPC is healthy" / Ctrl+C once ready
```

### 2. Register the server with Claude Code

From the repo root:

```bash
claude mcp add sova-miner -- node "$(pwd)/mcp/dist/index.js"
```

(Use `-s user` instead of the default `-s local` scope to make it available
across all your projects, not just this checkout.) Verify it connected:

```bash
claude mcp list
claude mcp get sova-miner
```

### 3. Converse

Open a Claude Code session in this repo and just talk:

> "Initialize a sova miner, fund it on regtest, then mine for 10 epochs at
> 100,000 zat each with a 2,000,000 zat budget, and give me a report when
> it's done."

Under the hood, that's roughly:

1. **`sova_init`** -- creates `mcp/.data/miner/keystore.json`, returns the
   t-addr to fund and the EVM address SIP-1 burns will credit: by default
   the keystore key's own Ethereum address. To spend that SOVA, *you* run
   `sova-miner --data-dir mcp/.data/miner export-evm-key --i-understand`
   and import the key into an EVM wallet; the server deliberately has no
   tool that exports keys. A `warnings` field in the result means the
   keystore predates this default and credits an unspendable address
   (fix: `sova-miner --data-dir mcp/.data/miner init --migrate-evm-address`).
2. **`sova_fund_regtest`** -- mines 101 blocks to that address (regtest
   coinbase needs 100 confirmations to mature).
3. Make sure something is producing blocks for the miner to react to --
   the miner only *submits burns in reaction to* new blocks, it never mines
   them itself. On a real regtest session, run
   `../box/regtest/auto-mine.sh 2` in another terminal (1 block every 2s),
   or have the agent run it as a background shell command.
4. **`sova_mine`** -- starts the budget-capped loop, returns a `runId`
   immediately.
5. **`sova_status`** (as many times as you like) -- watch epochs land.
6. **`sova_stop`** -- if you want to end it early; otherwise it stops on its
   own at `--max-epochs` or budget exhaustion.
7. **`sova_report`** -- ask for `--verify-rpc` to get an independent
   on-chain cross-check, not just the local ledger.

### 4. Tear down

```bash
cd ../box/regtest
docker compose down -v
```

## Proof this actually works

`docs/walkthrough.md` is a captured transcript of every tool being called in
sequence -- via `@modelcontextprotocol/sdk`'s `Client` over the same stdio
transport Claude Code uses, not a mock -- against a live regtest harness:
init, fund, mine (both a bounded run that completes on its own and an
unbounded run stopped mid-flight), status polling, stop, and a
chain-verified report. Reproduce it yourself:

```bash
npm run build
# with the regtest harness up and a fresh chain (docker compose down -v && up -d):
npm run driver
```

A recorded GIF of an actual Claude Code conversation doing this is Wave-2
content and out of scope for this task.

## Configuration (env vars, all optional)

| Var | Default | Purpose |
| --- | --- | --- |
| `SOVA_MINER_BIN` | `<repo>/crates/burn-wallet/target/release/sova-miner` | Override the miner binary path. |
| `SOVA_REPO_ROOT` | parent of `mcp/` | Override repo-root resolution. |
| `SOVA_MCP_DATA_DIR` | `mcp/.data` | Where keystores/state/run logs are written. |
| `SOVA_REGTEST_RPC_URL` | `http://127.0.0.1:18232` | Default RPC endpoint for tools/driver that take one. |

## Notes / known limitations

- **One active `mine` run at a time**, enforced in-process. State (pid, log
  path, status) lives in the server process's memory, not on disk -- if the
  server restarts, it forgets about a run it started (the child process
  itself is unaffected and keeps running/logging; you'd need to find and
  kill its pid manually, or just let it finish).
- `sova_fund_regtest` is a thin wrapper around zebrad's regtest-only
  `generatetoaddress` RPC. It will fail loudly (not silently no-op) if
  pointed at a non-regtest node, since `generatetoaddress`/`generate` are
  themselves gated to regtest server-side.
- No CLI-flag mismatches were found against `sova-miner --help`: every flag
  this server passes (`--data-dir`, `--network`, `--budget-zat`,
  `--per-epoch-zat`, `--rpc`, `--poll-interval-ms`, `--max-epochs`,
  `--evm-address`, `--verify-rpc`) matches the D2 CLI's own `--help` output
  exactly, including that `--data-dir`/`--network` are global flags valid
  both before and after the subcommand.
