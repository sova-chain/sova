#!/usr/bin/env node
// End-to-end for /history: the built site served statically and a headless
// Chromium reading it, against the real NEAR testnet and against a mock.
//
//   cd site && npm run build && cd e2e && npm i && node history.mjs
//
// Needs network (NEAR testnet RPCs, rpc-testnet.sova.io) and a Chromium:
// CHROME=, else playwright's cache, else /Applications/Google Chrome.app.
// PLAYWRIGHT=/path/to/node_modules/playwright-core to use another install.
// Screenshots go to $SHOTS (default ./shots).
//
// Checks:
//  A. NEAR testnet, the near-da worker's e2e contract (14 real batches, box
//     chain 1337) + a mock Sova RPC on chain 1337: every batch renders with
//     the right range, size, sha256 prefix and links; info, gap and rail;
//     "check" downloads the batch's tx from NEAR and matches its sha256.
//  B. Same contract + the public Sova RPC (chain 82330): the chain mismatch
//     is said, no gap.
//  C. The default contract (sova-da.testnet) + the public RPC: the archive
//     state renders (archive-starting while nothing is posted).
//  D. A mock NEAR that starts empty and grows to 45 batches: archive
//     starting, then rows arrive without a reload, "older batches" pages.
//  E. Failures: unreachable NEAR RPCs, unreachable Sova RPC, no such account,
//     a bad account name: each says what failed; retry shows.
//  F. 390 px: no horizontal scroll. noindex, out of the sitemap.
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import http from 'node:http';
import { createServer } from 'node:net';
import { homedir } from 'node:os';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const DIST = resolve(HERE, '../dist');
const SHOTS = process.env.SHOTS || join(HERE, 'shots');
const E2E_CONTRACT = 'e2e-20261004035415.sova-da.testnet';
const { chromium } = await import(
  process.env.PLAYWRIGHT ? pathToFileURL(join(process.env.PLAYWRIGHT, 'index.mjs')).href : 'playwright-core'
);

const servers = [];
process.on('exit', () => servers.forEach((s) => s.close()));
process.on('SIGINT', () => process.exit(130));

function assert(c, m) {
  if (!c) throw new Error(`ASSERT: ${m}`);
  console.log(`   ok  ${m}`);
}
const step = (m) => console.log(`\n== ${m}`);
const freePort = () =>
  new Promise((ok) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => {
      const p = s.address().port;
      s.close(() => ok(p));
    });
  });
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
  const mac = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
  if (existsSync(mac)) return mac;
  throw new Error('no Chromium found: set CHROME=/path/to/chrome');
}

function listen(handler) {
  return freePort().then(
    (port) =>
      new Promise((ok) => {
        const s = http.createServer(handler);
        servers.push(s);
        s.listen(port, '127.0.0.1', () => ok(`http://127.0.0.1:${port}`));
      }),
  );
}
const staticSite = () =>
  listen((req, res) => {
    const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2', '.png': 'image/png', '.xml': 'application/xml' };
    let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    if (p.endsWith('/')) p += 'index.html';
    else if (!extname(p)) p += '/index.html';
    const f = join(DIST, p);
    if (!f.startsWith(DIST) || !existsSync(f)) {
      res.writeHead(404);
      return res.end();
    }
    res.writeHead(200, { 'content-type': types[extname(f)] || 'application/octet-stream' });
    res.end(readFileSync(f));
  });

/** JSON-RPC server with browser CORS; `fn(method, params)` returns the result or throws {error}. */
const jsonRpc = (fn) =>
  listen((req, res) => {
    const cors = { 'access-control-allow-origin': '*', 'access-control-allow-headers': '*', 'access-control-allow-methods': 'POST, OPTIONS' };
    if (req.method === 'OPTIONS') {
      res.writeHead(204, cors);
      return res.end();
    }
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      const j = JSON.parse(body);
      let out;
      try {
        out = { jsonrpc: '2.0', id: j.id, result: fn(j.method, j.params) };
      } catch (e) {
        out = { jsonrpc: '2.0', id: j.id, error: e.error || { code: -32000, message: String(e) } };
      }
      res.writeHead(200, { 'content-type': 'application/json', ...cors });
      res.end(JSON.stringify(out));
    });
  });

/** A mock Sova RPC: chain id and head. */
const sovaMock = (chainId, head) =>
  jsonRpc((m) => {
    if (m === 'eth_chainId') return '0x' + chainId.toString(16);
    if (m === 'eth_blockNumber') return '0x' + head.toString(16);
    throw new Error(m);
  });

/** A mock NEAR RPC holding `state.count` batches of 40 blocks (chain 1337). */
function nearMock(state) {
  const CH = 1337n, N = 40;
  const bytesOf = (i) => {
    const b = Buffer.alloc(28 + 100);
    b.write('SOVADA1\0', 0, 'latin1');
    b.writeBigUInt64LE(CH, 8);
    b.writeBigUInt64LE(BigInt(i * N), 16);
    b.writeUInt32LE(N, 24);
    b.fill(i & 0xff, 28);
    return b;
  };
  const entry = (i) => ({
    index: i, first_height: i * N, last_height: i * N + N - 1, count: N, bytes: 128,
    sha256: createHash('sha256').update(bytesOf(i)).digest('hex'), last_hash: '0x' + '11'.repeat(32),
    near_block: 1000 + i, tx_hash: i % 7 === 3 ? null : `MockTx${i}`,
  });
  const enc = (v) => [...Buffer.from(JSON.stringify(v))];
  return jsonRpc((m, p) => {
    if (m === 'query') {
      const args = JSON.parse(Buffer.from(p.args_base64, 'base64').toString() || '{}');
      if (p.method_name === 'info')
        return { result: enc({ format: 'SOVADA1', owner: 'mock.testnet', chain_id: 1337, start_height: 0, next_height: state.count * N, batch_count: state.count, last_hash: state.count ? '0x' + '11'.repeat(32) : null, bytes_posted: state.count * 128 }) };
      if (p.method_name === 'batches') {
        const out = [];
        for (let i = args.from_index; i < Math.min(state.count, args.from_index + Math.min(args.limit, 100)); i++) out.push(entry(i));
        return { result: enc(out) };
      }
    }
    if (m === 'block') return { header: { timestamp_nanosec: String(BigInt(Date.now() - 600_000) * 1_000_000n) } };
    if (m === 'tx') {
      const i = Number(p.tx_hash.slice(6));
      return { transaction: { actions: [{ FunctionCall: { method_name: 'post', args: bytesOf(i).toString('base64') } }] } };
    }
    throw new Error(m);
  });
}

// ---- run ---------------------------------------------------------------------

mkdirSync(SHOTS, { recursive: true });
assert(existsSync(join(DIST, 'history/index.html')), 'site built (dist/history/index.html)');
const SITE = await staticSite();
const browser = await chromium.launch({ executablePath: findChrome() });
const errors = [];
async function open(query, viewport = { width: 1100, height: 1000 }) {
  const ctx = await browser.newContext({ viewport });
  const p = await ctx.newPage();
  p.on('pageerror', (e) => errors.push(e.message));
  await p.goto(`${SITE}/history${query ? '?' + query : ''}`);
  return p;
}
const text = (p, sel) => p.locator(sel).first().innerText();
const shot = (p, name) => p.screenshot({ path: join(SHOTS, `${name}.png`), fullPage: true });
const waitText = (p, sel, s, ms = 30000) =>
  p.waitForFunction(([sel, s]) => document.querySelector(sel)?.textContent.includes(s), [sel, s], { timeout: ms });

try {
  step('A: e2e contract on NEAR testnet, mock Sova RPC (chain 1337, head 512)');
  const sova1337 = await sovaMock(1337, 512);
  const a = await open(`contract=${E2E_CONTRACT}&rpc=${encodeURIComponent(sova1337)}`);
  await a.waitForFunction(() => document.querySelectorAll('#rows .row').length > 0, null, { timeout: 30000 });
  await waitText(a, '#last', 'UTC');
  await a.waitForTimeout(1200); // let the rows' entry animation finish
  const rows = a.locator('#rows .row');
  assert((await rows.count()) === 14, 'all 14 batches listed');
  const top = rows.first();
  assert((await top.locator('.ix').innerText()) === '13', 'newest first (#13)');
  assert((await top.locator('.rg').innerText()) === '190–191', 'Sova range 190–191');
  assert((await top.locator('.rg a').first().getAttribute('href')) === 'https://explorer.testnet.sova.io/block/190', 'range links the Sova explorer');
  assert((await top.locator('.cnt').innerText()).startsWith('2'), 'block count 2');
  assert((await top.locator('.sz').innerText()) === '1.4 KB', 'size 1.4 KB (1,438 B)');
  assert((await top.locator('.sha').innerText()) === '0f92e59fc0bd', 'sha256 prefix');
  assert(
    (await top.locator('.tx a').getAttribute('href')) === 'https://testnet.nearblocks.io/txns/8crZapXbajbXafT3aN9cVvs7VR1E7DuBBSCKwhHvo4SG',
    'tx links testnet.nearblocks.io/txns/<hash>',
  );
  assert((await rows.nth(7).locator('.sz').innerText()) === '814 KB', 'batch #6 is 814 KB');
  assert((await text(a, '#chain')) === '1337', 'chain id 1337');
  assert((await text(a, '#count')) === '14', '14 batches');
  assert((await text(a, '#thru')) === '#191', 'on NEAR through #191');
  assert((await text(a, '#head')) === '#512', 'head #512');
  assert((await text(a, '#gap')) === '321 blocks', 'gap 321 blocks');
  assert((await text(a, '#why')).includes('final on Sova'), 'gap explained');
  assert((await text(a, '#bytes')).includes('936 KB'), 'bytes posted');
  const rail = await a.$$eval('#rail i', (c) => c.map((x) => x.className));
  assert(rail.length === 64 && rail.includes('on') && rail.includes('wait') && rail[63].includes('head'), 'rail: on NEAR, waiting, head');
  assert((await a.locator('#status').getAttribute('data-k')) === 'ok', 'status ok');
  assert((await text(a, '#st-near')).includes('rpc.testnet.fastnear.com'), 'reads via rpc.testnet.fastnear.com');
  await shot(a, 'a-e2e-contract');
  await top.locator('.ck button').click();
  await a.waitForFunction(() => ['ok', 'err'].includes(document.querySelector('#rows .row .res')?.dataset.k), null, { timeout: 30000 });
  const res = await top.locator('.res').innerText();
  assert((await top.locator('.res').getAttribute('data-k')) === 'ok', `check #13: ${res}`);
  assert(res.includes('1,438 bytes') && res.includes('sha256 matches'), 'check downloaded the batch and matched sha256');
  await rows.nth(7).locator('.ck button').click(); // the 814 KB one
  await a.waitForFunction(() => ['ok', 'err'].includes(document.querySelectorAll('#rows .row')[7].querySelector('.res')?.dataset.k), null, { timeout: 60000 });
  assert((await rows.nth(7).locator('.res').getAttribute('data-k')) === 'ok', `check #6 (814 KB): ${await rows.nth(7).locator('.res').innerText()}`);
  await shot(a, 'a-checked');

  step('A390: same at 390 px');
  const m = await open(`contract=${E2E_CONTRACT}&rpc=${encodeURIComponent(sova1337)}`, { width: 390, height: 844 });
  await m.waitForFunction(() => document.querySelectorAll('#rows .row').length === 14, null, { timeout: 30000 });
  await m.waitForTimeout(1200);
  assert(await m.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'no horizontal scroll at 390 px');
  await shot(m, 'a-390');

  step('B: e2e contract + public Sova RPC (chain 82330): mismatch, no gap');
  const b = await open(`contract=${E2E_CONTRACT}`);
  await waitText(b, '#why', 'chain 1337');
  assert((await text(b, '#why')).includes('82330'), 'mismatch names both chains');
  assert((await text(b, '#gap')) === '—', 'no gap');
  assert(Number((await text(b, '#head')).replace(/[#,]/g, '')) > 60000, `public head ${await text(b, '#head')}`);

  step('C: default contract sova-da.testnet + public Sova RPC');
  const c = await open('');
  await c.waitForFunction(() => ['live', 'starting'].includes(document.getElementById('hist').dataset.state), null, { timeout: 30000 });
  const state = await c.locator('#hist').getAttribute('data-state');
  assert((await text(c, '#chain')) === '82330', 'chain 82330');
  if (state === 'starting') {
    assert(await c.locator('#empty').isVisible(), 'archive starting shown');
    assert((await text(c, '#thru')) === 'none yet', 'on NEAR through: none yet');
    assert(await c.$$eval('#rail i', (x) => !x.some((e) => e.className.includes('on')) && x[63].className.includes('head')), 'rail: nothing on NEAR yet, head');
  } else {
    assert((await c.locator('#rows .row').count()) > 0, 'batches listed');
    assert(/blocks$/.test(await text(c, '#gap')), `gap ${await text(c, '#gap')}`);
  }
  console.log(`   (sova-da.testnet state: ${state}, ${await text(c, '#count')} batches)`);
  await shot(c, 'c-default');

  step('D: mock NEAR, empty, then 45 batches; paging');
  const ns = { count: 0 };
  const nm = await nearMock(ns);
  const d = await open(`contract=mock.testnet&near=${encodeURIComponent(nm)}&rpc=${encodeURIComponent(await sovaMock(1337, 2100))}`);
  await d.waitForFunction(() => document.getElementById('hist').dataset.state === 'starting', null, { timeout: 15000 });
  assert(await d.locator('#empty').isVisible(), 'archive starting');
  assert((await text(d, '#last')) === 'none yet', 'last batch: none yet');
  await shot(d, 'd-starting');
  ns.count = 45;
  await d.waitForFunction(() => document.querySelectorAll('#rows .row').length === 20, null, { timeout: 25000 });
  assert(!(await d.locator('#empty').isVisible()), 'rows arrived without a reload');
  assert((await d.locator('#rows .row').first().locator('.ix').innerText()) === '44', 'newest #44 on top');
  assert((await text(d, '#gap')) === '301 blocks', 'gap 2100 - 1799 = 301');
  assert(await d.locator('#rows .row[data-index="38"] .tx a.dim').count() === 1, 'missing tx hash: links the NEAR block');
  await d.locator('#more').click();
  await d.waitForFunction(() => document.querySelectorAll('#rows .row').length === 40);
  await d.locator('#more').click();
  await d.waitForFunction(() => document.querySelectorAll('#rows .row').length === 45);
  assert(await d.locator('#more').isHidden(), 'older batches: paged to #0, button gone');
  const idx = await d.$$eval('#rows .row', (r) => r.map((x) => Number(x.dataset.index)));
  assert(idx.every((v, i) => v === 44 - i), 'rows 44..0 in order, no duplicates');
  await d.locator('#rows .row[data-index="44"] .ck button').click();
  await waitText(d, '#rows .row[data-index="44"] .res', 'sha256 matches');
  assert(true, 'check against mock tx');
  ns.count = 47;
  await d.waitForFunction(() => document.querySelectorAll('#rows .row').length === 47, null, { timeout: 25000 });
  assert((await d.locator('#rows .row').first().locator('.ix').innerText()) === '46', 'new batches prepend');

  step('E: failures');
  const dead = `http://127.0.0.1:${await freePort()}`;
  const e1 = await open(`near=${encodeURIComponent(dead)}`);
  await waitText(e1, '#st-near', 'unreachable');
  assert((await e1.locator('#status').getAttribute('data-k')) === 'err', 'NEAR down: status err');
  assert((await text(e1, '#st-near')).includes('127.0.0.1'), `says which RPC: ${await text(e1, '#st-near')}`);
  assert(await e1.locator('#retry').isVisible(), 'retry shown');
  await shot(e1, 'e-near-down');
  const e2 = await open(`contract=${E2E_CONTRACT}&rpc=${encodeURIComponent(dead)}`);
  await waitText(e2, '#st-sova', 'unreachable');
  await e2.waitForFunction(() => document.querySelectorAll('#rows .row').length === 14, null, { timeout: 30000 });
  assert(true, 'Sova RPC down: NEAR data still shows, the head failure is named');
  const e3 = await open('contract=nope-xyz-123.testnet');
  await waitText(e3, '#st-near', 'no such NEAR account');
  assert(true, 'unknown account named');
  const e4 = await open('contract=Bad..Name');
  await waitText(e4, '#st-near', 'not a NEAR account name', 5000);
  assert(true, 'bad account name refused before any call');

  step('F: noindex, not in sitemap');
  const html = readFileSync(join(DIST, 'history/index.html'), 'utf8');
  assert(html.includes('<meta name="robots" content="noindex">'), 'noindex');
  assert(!readFileSync(join(DIST, 'sitemap.xml'), 'utf8').includes('/history'), 'not in sitemap');
  assert(errors.length === 0, `no page errors${errors.length ? ': ' + errors.join(' | ') : ''}`);
  console.log(`\nPASS (screenshots in ${SHOTS})`);
} finally {
  await browser.close();
}
process.exit(0);
