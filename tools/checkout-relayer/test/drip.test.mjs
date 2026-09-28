// The testnet SOVA drip (POST /drip): the bookkeeping on a fake clock, then
// the route against a real Ashwings on anvil: a new address gets exactly one
// mint plus gas and can mint with it; repeats, rich addresses, owl holders,
// busy IPs and an empty pot are refused; the limits survive a restart; it
// is off by default and refuses to start on a chain not listed as a test
// network.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, describe, test } from 'node:test';
import { createWalletClient, http, parseAbi } from 'viem';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { foundry } from 'viem/chains';
import { DripBook } from '../src/drip.mjs';
import { PKG, acct, keyOf, missing, startChain, startRelayer } from './harness.mjs';

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

describe('DripBook', () => {
  const book = (o = {}) => {
    let t = 1_000_000_000_000;
    const b = new DripBook({ addressCooldownMs: DAY, perIpPerDay: 2, perHour: 3, perDay: 5, now: () => t, ...o });
    return { b, tick: (ms) => (t += ms) };
  };

  test('one drip per address per cooldown, case-insensitive', () => {
    const { b, tick } = book();
    assert.equal(b.check('0xAA', 'ip1'), null);
    b.record('0xAA', 'ip1');
    assert.equal(b.check('0xaa', 'ip2').code, 'address_cooldown');
    tick(DAY - 1000);
    assert.equal(b.check('0xaa', 'ip2').retryAfter, 1);
    tick(1000);
    assert.equal(b.check('0xaa', 'ip2'), null);
  });

  test('per IP per day, then hourly and daily caps for everyone', () => {
    const { b, tick } = book();
    b.record('0x1', 'ip1');
    b.record('0x2', 'ip1');
    assert.equal(b.check('0x3', 'ip1').code, 'ip_cooldown');
    b.record('0x3', 'ip2');
    const h = b.check('0x4', 'ip3');
    assert.equal(h.code, 'hourly_cap');
    assert.ok(h.retryAfter > 3500 && h.retryAfter <= 3600);
    tick(HOUR + 1);
    b.record('0x4', 'ip3');
    b.record('0x5', 'ip4');
    assert.equal(b.check('0x6', 'ip5').code, 'daily_cap');
  });

  test('forget gives the slot back; the log round-trips without IPs', () => {
    const { b } = book();
    const e = b.record('0x1', 'ip1');
    b.forget(e, 'ip1');
    assert.equal(b.check('0x1', 'ip1'), null);
    b.record('0x2', 'ip9').txHash = '0xabc';
    const saved = JSON.parse(JSON.stringify(b));
    assert.deepEqual(Object.keys(saved[0]).sort(), ['addr', 'at', 'txHash']);
    assert.ok(!JSON.stringify(saved).includes('ip9'), 'no IPs on disk');
    const { b: b2 } = book();
    b2.load(saved);
    assert.equal(b2.check('0x2', 'other').code, 'address_cooldown');
  });
});

const skip = missing();
const SITE = 'https://sova.io';
const TMP = mkdtempSync(join(tmpdir(), 'relayer-drip-'));
const PRICE = 10n ** 19n; // the harness Ashwings: 10 SOVA
const GAS = 10n ** 17n; // DRIP_GAS_WEI default: 0.1 SOVA

async function post(url, body, { ip, origin = SITE } = {}) {
  const res = await fetch(`${url}/drip`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...(origin ? { origin } : {}), ...(ip ? { 'cf-connecting-ip': ip } : {}) },
    body: JSON.stringify(body),
  });
  return { status: res.status, headers: res.headers, body: await res.json().catch(() => null) };
}
const fresh = () => privateKeyToAccount(generatePrivateKey());

describe('POST /drip', { skip: skip ?? false, concurrency: false }, () => {
  let chain;
  let r;
  const state = join(TMP, 'state.json');
  const ENV = {
    RELAYER_KEY: keyOf(2),
    CORS_ORIGIN: SITE,
    TRUST_PROXY_HEADER: 'cf-connecting-ip',
    DRIP: '1',
    DRIP_PER_IP_PER_DAY: '2',
    STATE_FILE: state,
  };
  const ASHW = parseAbi([
    'function mint() payable returns (uint256)',
    'function balanceOf(address) view returns (uint256)',
    'function transferFrom(address from, address to, uint256 id)',
  ]);

  before(async () => {
    chain = await startChain();
    r = await startRelayer(chain, ENV);
  });
  after(async () => {
    await r?.stop();
    chain?.stop();
  });

  test('/status says the drip is on, what it gives, and its limits', async () => {
    const s = await (await fetch(`${r.url}/status`)).json();
    assert.equal(s.drip.accepting, true);
    assert.equal(BigInt(s.drip.maxWei), PRICE + GAS);
    assert.equal(s.drip.firstOwlOnly, true);
    assert.equal(s.drip.limits.perIpPerDay, 2);
  });

  let first;
  test('a new address gets one mint plus gas, and mints with it', async () => {
    first = fresh();
    const res = await post(r.url, { address: first.address }, { ip: '198.51.100.1' });
    assert.equal(res.status, 200, JSON.stringify(res.body));
    assert.equal(BigInt(res.body.amountWei), PRICE + GAS);
    assert.equal(res.headers.get('access-control-allow-origin'), SITE);
    assert.equal(await chain.pub.getBalance({ address: first.address }), PRICE + GAS);
    const w = createWalletClient({ chain: foundry, transport: http(chain.rpc), account: first });
    const rc = await chain.pub.waitForTransactionReceipt({
      hash: await w.writeContract({ address: chain.ashwings, abi: ASHW, functionName: 'mint', value: PRICE }),
    });
    assert.equal(rc.status, 'success');
    assert.equal(await chain.pub.readContract({ address: chain.ashwings, abi: ASHW, functionName: 'balanceOf', args: [first.address] }), 1n);
  });

  test('the same address again: 429 with Retry-After', async () => {
    const again = await post(r.url, { address: first.address.toLowerCase() }, { ip: '198.51.100.2' });
    assert.equal(again.status, 429);
    assert.match(again.body.error, /already got SOVA/);
    assert.ok(Number(again.headers.get('retry-after')) > 86_000);
  });

  test('an address that can already mint, or already has an owl: 409, nothing sent', async () => {
    const rich = await post(r.url, { address: acct(7).address }, { ip: '198.51.100.3' });
    assert.equal(rich.status, 409);
    assert.match(rich.body.error, /enough SOVA/);
    const holder = fresh();
    const w = createWalletClient({ chain: foundry, transport: http(chain.rpc), account: first });
    await chain.pub.waitForTransactionReceipt({
      hash: await w.writeContract({ address: chain.ashwings, abi: ASHW, functionName: 'transferFrom', args: [first.address, holder.address, 1n] }),
    });
    const owl = await post(r.url, { address: holder.address }, { ip: '198.51.100.3' });
    assert.equal(owl.status, 409);
    assert.match(owl.body.error, /already has an owl/);
    assert.equal(await chain.pub.getBalance({ address: holder.address }), 0n);
  });

  test('a partly funded address is topped up, not paid in full', async () => {
    const half = fresh();
    const w = createWalletClient({ chain: foundry, transport: http(chain.rpc), account: acct(7) });
    await chain.pub.waitForTransactionReceipt({ hash: await w.sendTransaction({ to: half.address, value: PRICE / 2n }) });
    const res = await post(r.url, { address: half.address }, { ip: '198.51.100.4' });
    assert.equal(res.status, 200, JSON.stringify(res.body));
    assert.equal(BigInt(res.body.amountWei), PRICE / 2n + GAS);
    assert.equal(await chain.pub.getBalance({ address: half.address }), PRICE + GAS);
  });

  test('per IP: DRIP_PER_IP_PER_DAY, then 429', async () => {
    const ip = '198.51.100.5';
    assert.equal((await post(r.url, { address: fresh().address }, { ip })).status, 200);
    assert.equal((await post(r.url, { address: fresh().address }, { ip })).status, 200);
    const third = await post(r.url, { address: fresh().address }, { ip });
    assert.equal(third.status, 429);
    assert.match(third.body.error, /your network/);
  });

  test('bad input and other origins are refused', async () => {
    assert.equal((await post(r.url, { address: '0x1234' })).status, 400);
    assert.equal((await post(r.url, { address: `0x${'0'.repeat(40)}` })).status, 400);
    assert.equal((await post(r.url, { address: fresh().address }, { origin: 'https://evil.example' })).status, 403);
  });

  test('the limits survive a restart (STATE_FILE keeps addresses and times, no IPs)', async () => {
    await r.stop();
    const saved = JSON.parse(readFileSync(state, 'utf8'));
    assert.equal(saved.drips.length, 4);
    assert.ok(!JSON.stringify(saved).includes('198.51.100'), 'no client IPs on disk');
    r = await startRelayer(chain, ENV);
    const again = await post(r.url, { address: first.address }, { ip: '198.51.100.9' });
    assert.equal(again.status, 429);
  });

  test('an empty pot refuses with 503 and keeps DRIP_KEEP_WEI for gas', async () => {
    const poorKey = generatePrivateKey();
    const poor = privateKeyToAccount(poorKey);
    const w = createWalletClient({ chain: foundry, transport: http(chain.rpc), account: acct(7) });
    await chain.pub.waitForTransactionReceipt({ hash: await w.sendTransaction({ to: poor.address, value: PRICE + GAS }) });
    // Enough for one drip, but not for one drip plus DRIP_KEEP_WEI.
    const p = await startRelayer(chain, { RELAYER_KEY: poorKey, CORS_ORIGIN: SITE, DRIP: '1' });
    try {
      const s = await (await fetch(`${p.url}/status`)).json();
      assert.equal(s.drip.accepting, false);
      assert.equal(s.drip.reason, 'empty');
      const res = await post(p.url, { address: fresh().address });
      assert.equal(res.status, 503);
      assert.match(res.body.error, /empty/);
    } finally {
      await p.stop();
    }
  });

  test('off by default: /drip is 404 and /status has no drip', async () => {
    const p = await startRelayer(chain, { RELAYER_KEY: keyOf(3), CORS_ORIGIN: SITE });
    try {
      assert.equal((await post(p.url, { address: fresh().address })).status, 404);
      assert.equal((await (await fetch(`${p.url}/status`)).json()).drip, null);
    } finally {
      await p.stop();
    }
  });

  test('testnet only: DRIP=1 on a chain not in DRIP_CHAIN_IDS refuses to start', async () => {
    const p = spawnSync(process.execPath, [join(PKG, 'src/server.mjs')], {
      env: { PATH: process.env.PATH, SOVA_RPC_URL: chain.rpc, ASHWINGS: chain.ashwings, RELAYER_KEY: keyOf(3), DRIP: '1', DRIP_CHAIN_IDS: '82330' },
      encoding: 'utf8',
      timeout: 20_000,
    });
    assert.notEqual(p.status, 0);
    assert.match(p.stderr, /not in DRIP_CHAIN_IDS/);
  });
});

if (skip) test(`chain tests skipped: ${skip}`, { skip: true }, () => {});
