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
//   D  /ashwings/mint, a newcomer: a wallet on Ethereum that has never seen
//      Sova and holds no SOVA. Connect: the switch fails the MetaMask-mobile
//      way (-32603 wrapping 4902), the page adds the chain, the user says no
//      -> "try again" -> added (this wallet adds without switching, so the
//      page switches too) -> step 2: the relayer's drip -> step 3: mint.
//      The wallet wanders off to another network -> "switch" -> back.
//      A second wallet (odd error code on switch) uses "+ add Sova testnet"
//      before connecting, then mints for SOVA; the gallery shows the owls
//   E  /ashwings/market: wallet D lists (approve + list), wallet E buys
//      (1% fee booked for the treasury), D lists and cancels another
//
// anvil runs as chain 82330 so the pages treat it as the public testnet
// (the wallet is asked to add "Sova testnet" with the explorer); its RPC
// is still the local one (?rpc=).
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
const [deployer, treasury, relayer, newcomer, buyerA, buyerB, buyerC, buyerD, buyerE] = [0, 1, 2, 3, 5, 6, 7, 8, 9].map(acct);
const PRICE_WEI = 10n ** 19n; // 10 SOVA
const PRICE_ZAT = 25_000_000n; // 0.25 ZEC
const CHAIN = { ...foundry, id: 82330 }; // the public testnet's id (infra/testnet/deployments/sova-testnet.json)
const SOVA_HEX = '0x1419a';
const EXPLORER = 'https://explorer.testnet.sova.io';

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
  if (!existsSync(join(OUT, 'AshwingsZecCheckout.sol'))) throw new Error('run `forge build` in contracts/ first');
  if (!existsSync(join(DIST, 'ashwings/buy/index.html'))) throw new Error('run `npm run build` in site/ first');
  mkdirSync(SHOTS, { recursive: true });

  step('anvil');
  const anvilPort = await freePort();
  const anvil = spawn('anvil', ['--port', String(anvilPort), '--chain-id', String(CHAIN.id), '--silent'], { stdio: 'ignore' });
  children.push(anvil);
  const RPC = `http://127.0.0.1:${anvilPort}`;
  const transport = viemHttp(RPC);
  const pub = createPublicClient({ chain: CHAIN, transport, pollingInterval: 100 });
  await waitFor(() => pub.getChainId(), 'anvil up');
  const wallet = (account) => createWalletClient({ chain: CHAIN, transport, account });
  const dep = wallet(deployer);
  const mined = async (hash) => {
    const r = await pub.waitForTransactionReceipt({ hash });
    if (r.status !== 'success') throw new Error(`tx reverted: ${hash}`);
    return r;
  };
  console.log(`   anvil ${RPC}`);

  step('deploy Ashwings (creates its ZEC checkout) + AshwingsMarket, etch MockZcash at 0x…5a00');
  const ashw = artifact('Ashwings.sol', 'Ashwings');
  const co = artifact('AshwingsZecCheckout.sol', 'AshwingsZecCheckout');
  const mkt = artifact('AshwingsMarket.sol', 'AshwingsMarket');
  const mock = artifact('MockZcash.sol', 'MockZcash');
  // The ZEC payee: a t-address of "the project" (any P2PKH hash will do here).
  const pkh = keccak256(toHex('sova demo seller')).slice(0, 42);
  const script = payeeScript(pkh, false);
  const sellerT = tAddr(pkh, false, 'test');
  const ashwAddr = (await mined(await dep.deployContract({
    abi: ashw.abi, bytecode: ashw.bytecode.object, args: [treasury.address, sellerT, PRICE_WEI, PRICE_ZAT],
  }))).contractAddress;
  const coAddr = await pub.readContract({ address: ashwAddr, abi: ashw.abi, functionName: 'zecCheckout' });
  assert(coAddr.toLowerCase() === getContractAddress({ from: ashwAddr, nonce: 1n }).toLowerCase(), `checkout at ${coAddr}, created by Ashwings`);
  const mktAddr = (await mined(await dep.deployContract({ abi: mkt.abi, bytecode: mkt.bytecode.object, args: [ashwAddr, treasury.address, 100] }))).contractAddress;
  console.log(`   ashwings ${ashwAddr} · market ${mktAddr} · zec payee ${sellerT}`);
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
      RPC_POLL_MS: '250', RPC_PER_10S: '1000', // anvil has no per-IP limit
      DRIP: '1', // the testnet SOVA drip for the mint page (chain 82330)
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

  const bad = await fetch(`${RELAYER}/reserve`, {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ listingId: 1, recipient: '0x' + '0'.repeat(40) }),
  });
  assert(bad.status === 400, 'relayer rejects a zero recipient');

  // ---- Flow D ------------------------------------------------------------
  step('D: /ashwings/mint — a newcomer: connect (add + switch, reject, retry), drip, mint');
  const Q = `rpc=${encodeURIComponent(RPC)}&ashw=${ashwAddr}&market=${mktAddr}&relayer=${encodeURIComponent(RELAYER)}`;
  const MINT = `http://127.0.0.1:${sitePort}/ashwings/mint/?${Q}`;
  const MKT = `http://127.0.0.1:${sitePort}/ashwings/market/?${Q}`;
  const walletOf = (account) => ({ fn: shim.fn, arg: { rpc: RPC, account: account.address } });
  const stHas = (p, t, ms) =>
    p.waitForFunction((x) => document.querySelector('#status .st').textContent.includes(x), t, { timeout: ms ?? 20000 });
  const stepIs = (p, id, want) => p.waitForFunction(([i, w]) => document.getElementById(i).dataset.s === w, [id, want], { timeout: 20000 });
  const ashwRead = (functionName, args = []) => pub.readContract({ address: ashwAddr, abi: ashw.abi, functionName, args });
  const mktRead = (functionName, args = []) => pub.readContract({ address: mktAddr, abi: mkt.abi, functionName, args });

  /**
   * A browser wallet that behaves like the real ones: its own chain list and
   * current chain, chainChanged/accountsChanged events, a configurable
   * "unknown chain" error on switch, and a user who can say no. Every call
   * lands in window.__walletLog.
   */
  const mockWallet = {
    fn: (o) => {
      const listeners = {};
      const emit = (ev, v) => (listeners[ev] || []).forEach((f) => f(v));
      const st = { chain: o.start, known: new Set(o.known), connected: false, rejectAdd: o.rejectAdd || 0 };
      const log = (window.__walletLog = []);
      const fwd = async (method, params) => {
        const r = await fetch(o.rpc, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
        const j = await r.json();
        if (j.error) throw j.error;
        return j.result;
      };
      window.__wallet = { st, goTo: (id) => { st.chain = id; emit('chainChanged', id); } };
      window.ethereum = {
        isMetaMask: true,
        on: (ev, f) => (listeners[ev] ||= []).push(f),
        removeListener: () => {},
        async request({ method, params = [] }) {
          log.push({ method, params });
          switch (method) {
            case 'eth_chainId':
              return st.chain;
            case 'eth_accounts':
              return st.connected ? [o.account] : [];
            case 'eth_requestAccounts':
              st.connected = true;
              emit('accountsChanged', [o.account]);
              return [o.account];
            case 'wallet_switchEthereumChain': {
              const id = params[0].chainId;
              if (!st.known.has(id)) throw o.switchError;
              if (st.chain !== id) window.__wallet.goTo(id);
              return null;
            }
            case 'wallet_addEthereumChain':
              if (st.rejectAdd > 0) {
                st.rejectAdd--;
                throw { code: 4001, message: 'User rejected the request.' };
              }
              st.known.add(params[0].chainId);
              if (o.addSwitches) window.__wallet.goTo(params[0].chainId);
              return null;
            case 'eth_sendTransaction':
              if (st.chain !== o.sova) throw { code: -32603, message: `sent on chain ${st.chain}` };
              return fwd(method, params);
            default:
              return fwd(method, params);
          }
        },
      };
    },
  };
  const walletLog = (p) => p.evaluate(() => window.__walletLog);

  // The newcomer: an anvil account emptied to 0, so it holds no SOVA.
  await pub.request({ method: 'anvil_setBalance', params: [newcomer.address, '0x0'] });
  const d = await newPage({}, {
    fn: mockWallet.fn,
    arg: {
      rpc: RPC, account: newcomer.address, sova: SOVA_HEX, start: '0x1', known: ['0x1'], rejectAdd: 1, addSwitches: false,
      // MetaMask mobile: an internal error that wraps 4902.
      switchError: { code: -32603, message: 'Unrecognized chain ID "0x1419a".', data: { originalError: { code: 4902 } } },
    },
  });
  await d.goto(MINT);
  await stHas(d, 'ready · connect');
  assert((await d.textContent('#n')) === '2', 'supply 2 (the two ZEC owls)');
  assert((await d.textContent('#p-sova')) === '10' && (await d.textContent('#p-zec')) === '0.25', 'prices 10 SOVA / 0.25 ZEC from the contract');
  assert((await d.getAttribute('#go-zec', 'href')).toLowerCase().includes(`co=${coAddr}`.toLowerCase()), 'no-wallet link hands off to /ashwings/buy with the checkout');
  assert(await d.isVisible('#b-connect.go') && await d.isVisible('#b-add'), 'step 1: connect (primary) and "+ add Sova testnet"');
  assert(await d.isDisabled('#go-sova'), 'step 3 waits');
  await d.waitForSelector('#grid li:nth-child(2) img[src^="data:image/svg+xml"]');
  await d.waitForTimeout(600);
  await shot(d, '10-mint-start');

  await d.click('#b-connect');
  await stHas(d, 'rejected in wallet');
  await d.waitForFunction(() => document.getElementById('b-connect').textContent.trim() === 'try again');
  const added = (await walletLog(d)).find((c) => c.method === 'wallet_addEthereumChain');
  assert(added, 'switch failed with -32603/4902, so the page asked to add the chain');
  assert(JSON.stringify(added.params[0]) === JSON.stringify({
    chainId: SOVA_HEX, chainName: 'Sova testnet', nativeCurrency: { name: 'SOVA', symbol: 'SOVA', decimals: 18 }, rpcUrls: [RPC], blockExplorerUrls: [EXPLORER],
  }), `add params: ${JSON.stringify(added.params[0])}`);
  assert((await d.textContent('#i1')).includes('another network'), 'step 1 shows the wrong network');
  await shot(d, '11-rejected-try-again');

  await d.click('#b-connect');
  await stepIs(d, 's1', 'done');
  const log1 = (await walletLog(d)).map((c) => c.method);
  assert(log1.filter((m) => m === 'wallet_addEthereumChain').length === 2, 'retry added the chain');
  assert(log1.lastIndexOf('wallet_switchEthereumChain') > log1.lastIndexOf('wallet_addEthereumChain'), 'added without switching, so the page switched after');
  assert((await d.evaluate(() => window.__wallet.st.chain)) === SOVA_HEX, 'wallet on chain 82330');
  await stepIs(d, 's2', 'cur');
  await d.waitForSelector('#b-drip.go:not([hidden])');
  assert((await d.textContent('#b-drip')).includes('get 10 test SOVA'), 'step 2: get 10 test SOVA');
  await shot(d, '12-connected-get-sova');

  await d.click('#b-drip');
  await stHas(d, 'now mint');
  const got = await pub.getBalance({ address: newcomer.address });
  assert(got === PRICE_WEI + 10n ** 17n, `drip: newcomer holds ${got} wei = price + 0.1 SOVA`);
  await stepIs(d, 's3', 'cur');
  await shot(d, '13-got-sova');

  await d.evaluate(() => window.__wallet.goTo('0x1'));
  await stepIs(d, 's1', 'cur');
  await d.waitForFunction(() => document.getElementById('b-connect').textContent.includes('switch to Sova testnet'));
  assert(await d.isDisabled('#go-sova'), 'wrong network: mint waits');
  await shot(d, '14-wrong-network');
  await d.click('#b-connect');
  await stepIs(d, 's3', 'cur');

  await d.click('#go-sova');
  await stHas(d, 'minted');
  await d.waitForSelector('#hero.mine #hero-img[src^="data:image/svg+xml"]');
  assert((await owner(3n)).toLowerCase() === newcomer.address.toLowerCase(), 'Ashwing #3 minted to the newcomer, paid with dripped SOVA');
  assert((await pub.getBalance({ address: ashwAddr })) === PRICE_WEI, 'exactly 10 SOVA collected by Ashwings');
  assert((await d.textContent('#hero-cap')) === '#3 · yours', 'the hero shows the new owl');
  assert((await d.getAttribute('#i3 a', 'href')).startsWith(`${EXPLORER}/tx/0x`), 'link to the mint tx on the explorer');
  await d.waitForSelector('#grid li:nth-child(3)');
  await d.waitForTimeout(700);
  await shot(d, '15-minted');
  const again = await fetch(`${RELAYER}/drip`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ address: newcomer.address }) });
  assert(again.status === 429, 'the drip gives an address one top-up per day');

  step('D2: "+ add Sova testnet" before connecting, then mint for SOVA');
  const d2 = await newPage({}, {
    fn: mockWallet.fn,
    arg: {
      rpc: RPC, account: buyerD.address, sova: SOVA_HEX, start: '0x2105', known: ['0x1', '0x2105'], addSwitches: true,
      // A wallet with its own idea of an error code for "unknown chain".
      switchError: { code: -32000, message: 'Chain 0x1419a is not supported' },
    },
  });
  await d2.goto(MINT);
  await stHas(d2, 'ready · connect');
  await d2.click('#b-add');
  await stHas(d2, 'Sova testnet is in your wallet');
  assert((await d2.evaluate(() => window.__wallet.st.chain)) === SOVA_HEX && !(await d2.evaluate(() => window.__wallet.st.connected)), 'added and switched, no account shared yet');
  await d2.click('#b-connect');
  await stepIs(d2, 's3', 'cur');
  assert((await walletLog(d2)).filter((c) => c.method === 'wallet_addEthereumChain').length === 1, 'connect needed no second add');
  assert((await d2.textContent('#i2')).includes('SOVA'), 'step 2 done: already holds SOVA');
  await d2.click('#go-sova');
  await stHas(d2, 'minted');
  assert((await owner(4n)).toLowerCase() === buyerD.address.toLowerCase(), 'Ashwing #4 minted to wallet D for SOVA');
  assert((await d2.textContent('#n')) === '4', 'supply 4');
  await d2.waitForSelector('#grid li:nth-child(4)');

  // ---- Flow E ------------------------------------------------------------
  step('E: /ashwings/market — list (approve + list), buy (1% fee), cancel');
  const e1 = await newPage({}, walletOf(buyerD));
  await e1.goto(MKT);
  await stHas(e1, 'connect a wallet');
  await e1.click('#connect');
  await e1.waitForSelector('#mine li[data-id="4"] input');
  await e1.fill('#mine li[data-id="4"] input', '25');
  await e1.click('#mine li[data-id="4"] button');
  await stHas(e1, 'done');
  assert(await mktRead('isLive', [4n]), 'owl #4 listed and live');
  const [, listed] = await mktRead('listings', [4n]);
  assert(listed === 25n * 10n ** 18n, 'listed at 25 SOVA');
  await e1.waitForSelector('#sale li[data-id="4"]');
  await e1.waitForTimeout(600);
  await shot(e1, '16-listed');

  const e2 = await newPage({}, walletOf(buyerE));
  await e2.goto(MKT);
  await e2.waitForSelector('#sale li[data-id="4"] button.go');
  const dBefore = await pub.getBalance({ address: buyerD.address });
  await e2.click('#sale li[data-id="4"] button.go');
  await stHas(e2, 'done');
  assert((await owner(4n)).toLowerCase() === buyerE.address.toLowerCase(), 'wallet E owns #4');
  assert((await pub.getBalance({ address: buyerD.address })) - dBefore === 2475n * 10n ** 16n, 'seller got exactly 24.75 SOVA');
  assert((await mktRead('feesOwed')) === 25n * 10n ** 16n, '0.25 SOVA (1%) booked for the treasury');
  await e2.click('#connect');
  await e2.waitForSelector('#mine li[data-id="4"]');
  await e2.waitForTimeout(600);
  await shot(e2, '17-bought');

  await mined(await wallet(buyerD).writeContract({ address: ashwAddr, abi: ashw.abi, functionName: 'mint', value: PRICE_WEI }));
  await e1.reload();
  await e1.click('#connect');
  await e1.waitForSelector('#mine li[data-id="5"] input');
  await e1.fill('#mine li[data-id="5"] input', '5');
  await e1.click('#mine li[data-id="5"] button');
  await stHas(e1, 'done');
  assert(await mktRead('isLive', [5n]), 'owl #5 listed');
  await e1.waitForSelector('#mine li[data-id="5"] .price');
  await e1.click('#mine li[data-id="5"] button');
  await stHas(e1, 'canceling #5 · done');
  assert(!(await mktRead('isLive', [5n])), 'owl #5 canceled');

  const f = await newPage({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true });
  await f.goto(MINT);
  await f.waitForSelector('#grid li:nth-child(5) img[src^="data:image/svg+xml"]');
  await f.waitForFunction(() => document.getElementById('i1').textContent.includes('no wallet in this browser'));
  assert((await f.getAttribute('#i1 a', 'href')).startsWith('https://metamask.app.link/dapp/'), 'phone, no wallet: open this page in a wallet app');
  await f.waitForTimeout(600);
  await shot(f, '18-mobile-mint');
  assert((await f.evaluate(() => document.documentElement.scrollWidth)) <= 390, 'no horizontal scroll at 390px');
  assert((await ashwRead('totalSupply')) === 5n, 'supply 5');

  const g = await newPage();
  await g.goto(`http://127.0.0.1:${sitePort}/ashwings/`);
  assert((await g.getAttribute('a.mint', 'href')) === '/ashwings/mint', '/ashwings: one primary button to the mint');
  await shot(g, '19-ashwings');

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
