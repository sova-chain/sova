// Test harness: anvil + Ashwings (which creates its ZEC checkout) + MockZcash
// etched at the SIP-4 address, and relayer processes on free ports. Needs
// `anvil` on PATH and `forge build` in contracts/ (same as e2e/run.mjs).
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createPublicClient, createWalletClient, http, keccak256, toHex } from 'viem';
import { mnemonicToAccount } from 'viem/accounts';
import { foundry } from 'viem/chains';
import { tAddr } from '../src/zcash.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
export const PKG = resolve(HERE, '..');
const OUT = resolve(PKG, '../../contracts/out');
export const ZCASH = '0x0000000000000000000000000000000000005a00';
const MNEMONIC = 'test test test test test test test test test test test junk'; // anvil's public dev mnemonic
export const acct = (i) => mnemonicToAccount(MNEMONIC, { addressIndex: i });
export const keyOf = (i) => toHex(acct(i).getHdKey().privateKey);

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const freePort = () =>
  new Promise((ok) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => {
      const p = s.address().port;
      s.close(() => ok(p));
    });
  });
export async function waitFor(fn, what, ms = 20000) {
  const t0 = Date.now();
  for (;;) {
    try {
      if (await fn()) return;
    } catch {}
    if (Date.now() - t0 > ms) throw new Error(`timeout: ${what}`);
    await sleep(150);
  }
}

/** Why the chain tests can't run here, or null. */
export function missing() {
  if (spawnSync('anvil', ['--version']).status !== 0) return 'no anvil on PATH (foundry)';
  if (!existsSync(join(OUT, 'AshwingsZecCheckout.sol'))) return 'run `forge build` in contracts/ first';
  return null;
}

const artifact = (file, name) => JSON.parse(readFileSync(join(OUT, file, `${name}.json`), 'utf8'));
const children = new Set();
process.on('exit', () => {
  for (const c of children) if (c.exitCode === null) c.kill('SIGKILL');
});

export async function startChain() {
  const port = await freePort();
  const anvil = spawn('anvil', ['--port', String(port), '--silent'], { stdio: 'ignore' });
  children.add(anvil);
  const rpc = `http://127.0.0.1:${port}`;
  const transport = http(rpc);
  const pub = createPublicClient({ chain: foundry, transport, pollingInterval: 100 });
  await waitFor(() => pub.getChainId(), 'anvil up');
  const dep = createWalletClient({ chain: foundry, transport, account: acct(0) });
  const mined = async (hash) => {
    const r = await pub.waitForTransactionReceipt({ hash });
    if (r.status !== 'success') throw new Error(`tx reverted: ${hash}`);
    return r;
  };
  const ashw = artifact('Ashwings.sol', 'Ashwings');
  const mock = artifact('MockZcash.sol', 'MockZcash');
  const sellerT = tAddr(keccak256(toHex('sova demo seller')).slice(0, 42), false, 'test');
  const ashwings = (await mined(await dep.deployContract({
    abi: ashw.abi, bytecode: ashw.bytecode.object, args: [acct(1).address, sellerT, 10n ** 19n, 5_000_000n],
  }))).contractAddress;
  const checkout = await pub.readContract({ address: ashwings, abi: ashw.abi, functionName: 'zecCheckout' });
  await pub.request({ method: 'anvil_setCode', params: [ZCASH, mock.deployedBytecode.object] });
  const zsend = async (functionName, args) =>
    mined(await dep.writeContract({ address: ZCASH, abi: mock.abi, functionName, args }));
  await zsend('init', [3_000_000n, 3_000_100n, 1_700_000_000]);
  return {
    rpc, pub, ashwings, checkout, zsend,
    /** Zcash blocks pass (Sova's anchor moves). */
    zmine: (n) => zsend('mine', [BigInt(n)]),
    stop: () => anvil.kill('SIGTERM'),
  };
}

/** A relayer process; resolves once /health answers. */
export async function startRelayer(chain, env) {
  const port = await freePort();
  const url = `http://127.0.0.1:${port}`;
  const logs = [];
  const p = spawn(process.execPath, [join(PKG, 'src/server.mjs')], {
    env: {
      PATH: process.env.PATH, SOVA_RPC_URL: chain.rpc, ASHWINGS: chain.ashwings, PORT: String(port),
      RPC_POLL_MS: '200', PRUNE_MS: '500', RPC_PER_10S: '1000', ...env,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  children.add(p);
  p.stdout.on('data', (d) => logs.push(String(d)));
  p.stderr.on('data', (d) => logs.push(String(d)));
  try {
    await waitFor(async () => (await fetch(`${url}/health`)).ok, 'relayer up');
  } catch (e) {
    p.kill('SIGKILL');
    throw new Error(`${e.message}\n${logs.join('')}`);
  }
  return {
    url, logs,
    stop: () => new Promise((ok) => {
      if (p.exitCode !== null) return ok();
      p.once('exit', ok);
      p.kill('SIGTERM');
    }),
  };
}
