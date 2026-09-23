#!/usr/bin/env node
// End-to-end: /ashwings/buy + relayer against anvil, with MockZcash etched
// at the SIP-4 address standing in for the node's precompile, and a fake
// zebrad for the watcher. No SIP-4 node needed.
//
// Prereqs: `forge build` in contracts/, `npm run build` in site/, a
// Chromium (playwright's cache or CHROME=/path/to/chrome).
//   node e2e/run.mjs            # screenshots to $SHOTS (default ./e2e/shots)
//
// Flows:
//   A  relayer reserves -> QR (decoded and checked) -> paste txid ->
//      1/3 conf -> claimable -> relayer claims -> owl
//   B  injected wallet reserves -> watcher finds the payment on "zebrad"
//      (payee output at vout 1) and claims -> page shows the owl untouched
//   C  phone width; wrong amount paid -> page says so
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync } from 'node:fs';
import http from 'node:http';
import { createServer } from 'node:net';
import { homedir } from 'node:os';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import jsQR from 'jsqr';
import { chromium } from 'playwright-core';
import {
  createPublicClient, createWalletClient, getContractAddress, http as viemHttp, keccak256, parseAbi, toHex,
} from 'viem';
import { mnemonicToAccount } from 'viem/accounts';
import { foundry } from 'viem/chains';
import { payeeScript, tAddr } from '../src/zcash.mjs';
import { fakeZebrad } from './fake-zebrad.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '../../..');
const OUT = join(REPO, 'contracts/out');
const DIST = join(REPO, 'site/dist');
const SHOTS = process.env.SHOTS || join(HERE, 'shots');
const ZCASH = '0x0000000000000000000000000000000000005a00';
const MNEMONIC = 'test test test test test test test test test test test junk'; // anvil's public dev mnemonic
const acct = (i) => mnemonicToAccount(MNEMONIC, { addressIndex: i });
const [deployer, seller, relayer, buyerA, buyerB, buyerC] = [0, 1, 2, 5, 6, 7].map(acct);

const children = [];
const servers = [];
function cleanup() {
  for (const c of children) if (c.exitCode === null) c.kill('SIGTERM');
  for (const s of servers) s.close();
}
process.on('exit', cleanup);
process.on('SIGINT', () => process.exit(130));

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const step = (m) => console.log(`\n== ${m}`);
function assert(c, m) {
  if (!c) throw new Error(`ASSERT: ${m}`);
  console.log(`   ok  ${m}`);
}
const freePort = () =>
  new Promise((ok) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => {
      const p = s.address().port;
      s.close(() => ok(p));
    });
  });
const artifact = (file, name) => JSON.parse(readFileSync(join(OUT, file, `${name}.json`), 'utf8'));
async function waitFor(fn, what, ms = 20000) {
  const t0 = Date.now();
  for (;;) {
    try {
      if (await fn()) return;
    } catch {}
    if (Date.now() - t0 > ms) throw new Error(`timeout: ${what}`);
    await sleep(250);
  }
}

function findChrome() {
  if (process.env.CHROME) return process.env.CHROME;
  const cache = join(homedir(), 'Library/Caches/ms-playwright');
  for (const d of existsSync(cache) ? readdirSync(cache).sort().reverse() : []) {
    const p = d.startsWith('chromium_headless_shell')
      ? join(cache, d, 'chrome-headless-shell-mac-arm64/chrome-headless-shell')
      : d.startsWith('chromium-')
        ? join(cache, d, 'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing')
        : '';
    if (p && existsSync(p)) return p;
  }
  throw new Error('no Chromium found: set CHROME=/path/to/chrome');
}

function staticServer(root, port) {
  const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2', '.png': 'image/png' };
  const s = http.createServer((req, res) => {
    let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    if (p.endsWith('/')) p += 'index.html';
    else if (!extname(p)) p += '/index.html';
    const f = join(root, p);
    if (!f.startsWith(root) || !existsSync(f)) {
      res.writeHead(404);
      return res.end();
    }
    res.writeHead(200, { 'content-type': types[extname(f)] || 'application/octet-stream' });
    res.end(readFileSync(f));
  });
  servers.push(s);
  return new Promise((ok) => s.listen(port, '127.0.0.1', ok));
}

// ---------------------------------------------------------------------------

async function main() {
  if (!existsSync(join(OUT, 'ZecCheckout.sol'))) throw new Error('run `forge build` in contracts/ first');
  if (!existsSync(join(DIST, 'ashwings/buy/index.html'))) throw new Error('run `npm run build` in site/ first');
  mkdirSync(SHOTS, { recursive: true });

  step('anvil');
  const anvilPort = await freePort();
  const anvil = spawn('anvil', ['--port', String(anvilPort), '--silent'], { stdio: 'ignore' });
  children.push(anvil);
  const RPC = `http://127.0.0.1:${anvilPort}`;
  const transport = viemHttp(RPC);
  const pub = createPublicClient({ chain: foundry, transport, pollingInterval: 100 });
  await waitFor(() => pub.getChainId(), 'anvil up');
  const wallet = (account) => createWalletClient({ chain: foundry, transport, account });
  const dep = wallet(deployer);
  const mined = async (hash) => {
    const r = await pub.waitForTransactionReceipt({ hash });
    if (r.status !== 'success') throw new Error(`tx reverted: ${hash}`);
    return r;
  };
  console.log(`   anvil ${RPC}`);

  step('deploy Ashwings + AshwingsZecCheckout, etch MockZcash at 0x…5a00');
  const ashw = artifact('Ashwings.sol', 'Ashwings');
  const co = artifact('ZecCheckout.sol', 'AshwingsZecCheckout');
  const mock = artifact('MockZcash.sol', 'MockZcash');
  const ashwAddr = (await mined(await dep.deployContract({ abi: ashw.abi, bytecode: ashw.bytecode.object }))).contractAddress;
  const coAddr = (await mined(await dep.deployContract({ abi: co.abi, bytecode: co.bytecode.object, args: [ashwAddr] }))).contractAddress;
  assert(coAddr.toLowerCase() === getContractAddress({ from: deployer.address, nonce: 1n }).toLowerCase(), `checkout at ${coAddr} (the page default)`);
  await pub.request({ method: 'anvil_setCode', params: [ZCASH, mock.deployedBytecode.object] });
  const Z = { address: ZCASH, abi: mock.abi };
  const zsend = async (functionName, args) => mined(await dep.writeContract({ ...Z, functionName, args }));
  await zsend('init', [3_000_000n, 3_000_100n, 1_700_000_000]);

  const fz = fakeZebrad();
  const zPort = await freePort();
  servers.push(fz.server);
  await new Promise((ok) => fz.server.listen(zPort, '127.0.0.1', ok));
  const anchor = async () => (await pub.readContract({ ...Z, functionName: 'anchor' }))[0];
  fz.state.tip = Number(await anchor());
  /** A Zcash block passes: the anchor (Sova's view) and zebrad's tip both move. */
  const zmine = async (n) => {
    await zsend('mine', [BigInt(n)]);
    fz.state.tip += n;
  };

  step('seller lists: 0.25 ZEC, window 40, minConf 3');
  const pkh = keccak256(toHex('sova demo seller')).slice(0, 42);
  const script = payeeScript(pkh, false);
  const sellerT = tAddr(pkh, false, 'test');
  await mined(await wallet(seller).writeContract({ address: coAddr, abi: co.abi, functionName: 'list', args: [25_000_000n, pkh, false, 40, 3] }));
  console.log(`   seller ${sellerT}`);

  /** A Zcash tx mined in the next block, visible to Sova's precompile and (optionally) to zebrad. */
  async function zcashPay(outputs, { zebrad = false } = {}) {
    const txid = keccak256(toHex(`zcash tx ${Math.random()}`));
    const height = (await anchor()) + 1n;
    await zsend('addTx', [txid, height, 5]);
    for (const o of outputs) await zsend('addOutput', [txid, o.value, `0x${o.script}`]);
    if (zebrad) fz.addTx(txid.slice(2), Number(height), outputs);
    return txid.slice(2);
  }

  step('relayer (watcher on, fake zebrad)');
  const relPort = await freePort();
  const RELAYER = `http://127.0.0.1:${relPort}`;
  const rel = spawn(process.execPath, [join(HERE, '../src/server.mjs')], {
    env: {
      ...process.env, SOVA_RPC_URL: RPC, CHECKOUT: coAddr, RELAYER_KEY: toHex(relayer.getHdKey().privateKey),
      PORT: String(relPort), ZCASH_RPC_URL: `http://127.0.0.1:${zPort}`, ZCASH_NET: 'test', POLL_MS: '1000',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  children.push(rel);
  rel.stdout.on('data', (d) => process.stdout.write(`   [relayer] ${d}`));
  rel.stderr.on('data', (d) => process.stdout.write(`   [relayer!] ${d}`));
  await waitFor(async () => (await fetch(`${RELAYER}/health`)).ok, 'relayer up');

  const sitePort = await freePort();
  await staticServer(DIST, sitePort);
  const PAGE = `http://127.0.0.1:${sitePort}/ashwings/buy/?rpc=${encodeURIComponent(RPC)}&co=${coAddr}&relayer=${encodeURIComponent(RELAYER)}`;

  const browser = await chromium.launch({ executablePath: findChrome() });
  const errors = [];
  const newPage = async (opts = {}, init) => {
    const ctx = await browser.newContext({ viewport: { width: 1100, height: 1000 }, ...opts });
    if (init) await ctx.addInitScript(init.fn, init.arg);
    const p = await ctx.newPage();
    p.on('pageerror', (e) => errors.push(e.message));
    p.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
    return p;
  };
  const shot = async (p, name) => {
    const f = join(SHOTS, `${name}.png`);
    await p.screenshot({ path: f, fullPage: true });
    console.log(`   shot ${f} (${(statSync(f).size / 1024).toFixed(0)} KB)`);
  };
  const statusHas = (p, s, ms) =>
    p.waitForFunction((t) => document.getElementById('st').textContent.includes(t), s, { timeout: ms ?? 20000 });
  const owner = (id) => pub.readContract({ address: ashwAddr, abi: parseAbi(['function ownerOf(uint256) view returns (address)']), functionName: 'ownerOf', args: [id] });
  const quoteOf = async (id) => (await pub.readContract({ address: coAddr, abi: co.abi, functionName: 'reservations', args: [id] }))[1];

  // ---- Flow A ------------------------------------------------------------
  step('A: relayer reserve -> pay -> paste txid -> confirmations -> claim');
  const a = await newPage();
  await a.goto(PAGE);
  await statusHas(a, 'enter your address');
  await shot(a, '01-start');

  await a.fill('#addr', buyerA.address);
  await a.click('#reserve');
  await statusHas(a, 'waiting for payment');
  assert(new URL(a.url()).searchParams.get('r') === '1', 'order #1 in the URL');
  const qA = await quoteOf(1n);
  assert(qA === 25_000_001n, `quote = price + tag = ${qA} zat`);
  const uriA = `zcash:${sellerT}?amount=0.25000001`;
  assert((await a.textContent('#uri')) === uriA, `page URI ${uriA}`);
  const px = await a.evaluate(async () => {
    const svg = document.querySelector('#qr svg').outerHTML;
    const img = new Image();
    img.src = 'data:image/svg+xml;base64,' + btoa(svg);
    await img.decode();
    const c = document.createElement('canvas');
    c.width = c.height = 330;
    const g = c.getContext('2d');
    g.imageSmoothingEnabled = false;
    g.drawImage(img, 0, 0, 330, 330);
    return Array.from(g.getImageData(0, 0, 330, 330).data);
  });
  const decoded = jsQR(Uint8ClampedArray.from(px), 330, 330);
  assert(decoded?.data === uriA, `QR decodes to ${decoded?.data}`);
  await shot(a, '02-reserved-qr');

  const txA = await zcashPay([{ value: qA, script, addr: sellerT }]); // not given to zebrad: manual path
  await a.fill('#txid', txA);
  await a.click('#check');
  await statusHas(a, "not on Sova's zcash view yet");
  await shot(a, '03-txid-not-anchored');

  await zmine(1);
  await statusHas(a, '1/3 confirmations');
  await shot(a, '04-seen-1of3');

  await zmine(2);
  await statusHas(a, 'claimable');
  assert(await a.isEnabled('#claim'), 'claim button enabled');
  await shot(a, '05-claimable');

  await a.click('#claim');
  await statusHas(a, 'minted');
  await a.waitForSelector('#owl-img[src^="data:image/svg+xml"]');
  await a.waitForTimeout(700);
  await shot(a, '06-minted-owl');
  assert((await owner(1n)).toLowerCase() === buyerA.address.toLowerCase(), 'Ashwing #1 owned by buyer A');

  // ---- Flow B ------------------------------------------------------------
  step('B: injected wallet reserves; watcher finds the payment and claims');
  const shim = {
    fn: ({ rpc, account }) => {
      window.ethereum = {
        async request({ method, params = [] }) {
          if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [account];
          if (method === 'wallet_switchEthereumChain') return null;
          const r = await fetch(rpc, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
          const j = await r.json();
          if (j.error) throw j.error;
          return j.result;
        },
      };
    },
    arg: { rpc: RPC, account: buyerB.address },
  };
  const b = await newPage({}, shim);
  await b.goto(PAGE);
  await b.click('#connect');
  assert((await b.inputValue('#addr')).toLowerCase() === buyerB.address.toLowerCase(), 'wallet address filled in');
  await b.click('#reserve');
  await statusHas(b, 'waiting for payment');
  const qB = await quoteOf(2n);
  // Wallet sends change first, the payment second: the watcher must find vout 1.
  const txB = await zcashPay(
    [
      { value: 123_456n, script: payeeScript('0x' + '11'.repeat(20), false), addr: tAddr('0x' + '11'.repeat(20), false, 'test') },
      { value: qB, script, addr: sellerT },
    ],
    { zebrad: true },
  );
  await zmine(3);
  await statusHas(b, 'minted', 30000);
  await b.waitForSelector('#owl-img[src^="data:image/svg+xml"]');
  assert((await b.inputValue('#txid')) === txB, 'txid filled in (relayer /status or Claimed event)');
  const st = await (await fetch(`${RELAYER}/status/2`)).json();
  assert(st.detected?.vout === 1 && st.claim?.state === 'claimed', `watcher: vout ${st.detected?.vout}, ${st.claim?.state}`);
  assert((await owner(2n)).toLowerCase() === buyerB.address.toLowerCase(), 'Ashwing #2 owned by buyer B');
  await b.waitForTimeout(700);
  await shot(b, '07-wallet-watcher-minted');

  // ---- Flow C ------------------------------------------------------------
  step('C: phone width, wrong amount');
  const c = await newPage({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true });
  await c.goto(PAGE);
  await c.fill('#addr', buyerC.address);
  await c.click('#reserve');
  await statusHas(c, 'waiting for payment');
  await shot(c, '08-mobile-pay');
  const qC = await quoteOf(3n);
  const txC = await zcashPay([{ value: qC + 1n, script, addr: sellerT }]);
  await zmine(1);
  await c.fill('#txid', txC);
  await c.click('#check');
  await statusHas(c, 'no output of exactly');
  await shot(c, '09-mobile-wrong-amount');

  const bad = await fetch(`${RELAYER}/reserve`, { method: 'POST', body: JSON.stringify({ listingId: 1, recipient: '0x' + '0'.repeat(40) }) });
  assert(bad.status === 400, 'relayer rejects a zero recipient');

  await browser.close();
  const real = errors.filter((e) => !/favicon/.test(e));
  assert(real.length === 0, `no page errors${real.length ? ': ' + real.join(' | ') : ''}`);
  console.log('\nE2E PASS');
}

main()
  .then(() => {
    cleanup();
    process.exit(0);
  })
  .catch((e) => {
    console.error('\nE2E FAIL:', e);
    cleanup();
    process.exit(1);
  });
