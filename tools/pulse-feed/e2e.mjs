#!/usr/bin/env node
// End-to-end for /pulse: anvil + ZcashBlocks at 0x…5A01 + the system call
// played from real testnet samples (feed.mjs), the built site served
// statically, and a headless Chromium reading it. No SIP-7 node needed.
//
// Prereqs: `forge build` in contracts/, `npm run build` in site/, anvil on
// PATH, playwright-core (`npm i` here, or PLAYWRIGHT=/path/to/node_modules/playwright-core),
// a Chromium (playwright's cache or CHROME=).
//   node e2e.mjs            # screenshots to $SHOTS (default ./shots)
// Checks: the page shows the newest recorded block's Σ shielded exactly,
// updates live, hover rewinds every row, no horizontal scroll at 390 px,
// an unreachable RPC shows an error, publish() logs one event per height.
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import http from 'node:http';
import { createServer } from 'node:net';
import { homedir } from 'node:os';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { loadSamples, rpc, runFeed, ZCASH_BLOCKS } from './feed.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '../..');
const DIST = join(REPO, 'site/dist');
const SHOTS = process.env.SHOTS || join(HERE, 'shots');
const TOPIC = '0xa13343acdebcc5ad91f6b66b8851f7f8870975616bf4339a7c829bfa93952b53';
const { chromium } = await import(process.env.PLAYWRIGHT ? pathToFileURL(join(process.env.PLAYWRIGHT, 'index.mjs')).href : 'playwright-core');

const children = [];
const servers = [];
function cleanup() {
  for (const c of children) if (c.exitCode === null) c.kill('SIGTERM');
  for (const s of servers) s.close();
}
process.on('exit', cleanup);
process.on('SIGINT', () => process.exit(130));

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
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
async function waitFor(fn, what, ms = 20000) {
  const t0 = Date.now();
  for (;;) {
    try {
      if (await fn()) return;
    } catch {}
    if (Date.now() - t0 > ms) throw new Error(`timeout: ${what}`);
    await sleep(200);
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
  const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2', '.png': 'image/png', '.xml': 'application/xml' };
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
const zecText = (zat) => {
  const int = Math.floor(zat / 1e8).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${int}.${(zat % 1e8).toString().padStart(8, '0')}`;
};
const shielded = (b) => b.pools[1] + b.pools[2] + b.pools[3] + b.pools[5];

// ---------------------------------------------------------------------------

assert(existsSync(join(REPO, 'contracts/out/ZcashBlocks.sol/ZcashBlocks.json')), 'ZcashBlocks built');
assert(existsSync(join(DIST, 'pulse/index.html')), 'site built with /pulse');
mkdirSync(SHOTS, { recursive: true });

const [ap, wp] = [await freePort(), await freePort()];
const url = `http://127.0.0.1:${ap}`;
const anvil = spawn('anvil', ['--port', String(ap), '--silent'], { stdio: 'ignore' });
children.push(anvil);
await waitFor(() => rpc(url, 'eth_chainId'), 'anvil up');
await staticServer(DIST, wp);

const samples = loadSamples();
const PREFILL = 96;
const LIVE = 8;
const gas = { fill: [], live: [] };
const recorded = [];
const feed = runFeed({
  url, samples, prefill: PREFILL, intervalMs: 3000, publishEvery: 4, count: LIVE,
  log: (m) => console.log(`   feed ${m}`),
  onRecord: ({ block, receipt, live }) => {
    recorded.push(block);
    (live ? gas.live : gas.fill).push(Number(receipt.gasUsed));
  },
});
await waitFor(() => recorded.length >= PREFILL, 'prefill', 60000);

console.log('\n== desktop');
const browser = await chromium.launch({ executablePath: findChrome() });
const errors = [];
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
page.on('pageerror', (e) => errors.push(String(e)));
page.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
await page.goto(`http://127.0.0.1:${wp}/pulse/?rpc=${encodeURIComponent(url)}`);
await page.waitForSelector('#status[data-k="ok"]', { timeout: 15000 });
let top = recorded[recorded.length - 1];
await waitFor(async () => (await page.textContent('#big')) === zecText(shielded(recorded[recorded.length - 1])), 'hero = Σ shielded of newest');
top = recorded[recorded.length - 1];
assert((await page.textContent('#zh')) === '#' + top.height.toLocaleString('en-US'), `ticker shows zcash #${top.height}`);
assert((await page.$$('#feed li')).length === 8, 'feed shows 8 blocks');
const ironwood = await page.textContent('#v-ironwood');
assert(ironwood === zecText(top.pools[5]).slice(0, -6), `ironwood row ${ironwood}`);

const before = top.height;
await waitFor(async () => (await page.textContent('#zh')) !== '#' + before.toLocaleString('en-US'), 'a live Zcash block arrives', 15000);
await sleep(900); // let the tweens land
top = recorded[recorded.length - 1];
const heroNow = await page.textContent('#big');
assert(heroNow === zecText(shielded(top)), `hero updated live to ${heroNow}`);
await page.screenshot({ path: join(SHOTS, 'pulse-desktop.png'), fullPage: true });

// Hover a sparkline: every row shows that older block.
const box = await (await page.$('#s-ironwood')).boundingBox();
await page.mouse.move(box.x + box.width * 0.25, box.y + box.height / 2);
await sleep(900);
const hovered = await page.textContent('#zh');
assert(hovered !== '#' + top.height.toLocaleString('en-US'), `hover rewinds the ticker to ${hovered}`);
await page.screenshot({ path: join(SHOTS, 'pulse-desktop-hover.png'), fullPage: false });
await page.mouse.move(5, 5);

console.log('\n== mobile');
const mob = await browser.newPage({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true });
mob.on('pageerror', (e) => errors.push(String(e)));
await mob.goto(`http://127.0.0.1:${wp}/pulse/?rpc=${encodeURIComponent(url)}`);
await mob.waitForSelector('#status[data-k="ok"]', { timeout: 15000 });
await sleep(1200);
const overflow = await mob.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
assert(overflow <= 0, `no horizontal scroll at 390px (overflow ${overflow})`);
await mob.screenshot({ path: join(SHOTS, 'pulse-mobile.png'), fullPage: true });

console.log('\n== error state');
const bad = await browser.newPage({ viewport: { width: 1280, height: 700 } });
await bad.goto(`http://127.0.0.1:${wp}/pulse/?rpc=${encodeURIComponent('http://127.0.0.1:9')}`);
await bad.waitForSelector('#status[data-k="err"]', { timeout: 15000 });
assert(true, 'unreachable rpc shows an error line');

await feed;
console.log('\n== logs + gas');
const logs = await rpc(url, 'eth_getLogs', [{ address: ZCASH_BLOCKS, topics: [TOPIC], fromBlock: '0x0', toBlock: 'latest' }]);
assert(logs.length === recorded.length, `publish() emitted one ZcashBlock log per recorded block (${logs.length})`);
assert(new Set(logs.map((l) => l.topics[1])).size === logs.length, 'no height published twice');
assert(errors.length === 0, `no page errors${errors.length ? ': ' + errors.join(' | ') : ''}`);
const avg = (a) => Math.round(a.reduce((x, y) => x + y, 0) / a.length);
console.log(`   record() receipts (ring filling, incl. 21k intrinsic + calldata): avg ${avg([...gas.fill, ...gas.live])}`);
console.log(`\nPASS. screenshots in ${SHOTS}`);
await browser.close();
cleanup();
process.exit(0);
