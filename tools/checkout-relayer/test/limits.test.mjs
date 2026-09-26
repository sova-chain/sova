// The public-internet limits, against a real checkout on anvil (MockZcash
// stands in for SIP-4): CORS and request hygiene, per-IP and per-recipient
// limits, the open-order cap (and that it survives a restart and lapses at
// the deadline), the balance floor, the claim limit, the pending (202)
// answer, and the key tools (keygen, sweep).
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, describe, test } from 'node:test';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { PKG, acct, keyOf, missing, startChain, startRelayer, waitFor } from './harness.mjs';

const skip = missing();
const SITE = 'https://sova.io';
const TMP = mkdtempSync(join(tmpdir(), 'relayer-test-'));
const addr = (i) => `0x${(0x1000 + i).toString(16).padStart(40, '0')}`;

async function post(url, path, body, { ip, origin = SITE, headers = {} } = {}) {
  const res = await fetch(url + path, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      ...(origin ? { origin } : {}),
      ...(ip ? { 'cf-connecting-ip': ip } : {}),
      ...headers,
    },
    body: typeof body === 'string' ? body : JSON.stringify(body),
  });
  return { status: res.status, headers: res.headers, body: await res.json().catch(() => null) };
}
const getJson = async (url) => (await fetch(url)).json();

describe('checkout relayer limits', { skip: skip ?? false, concurrency: false }, () => {
  let chain;
  let r1;
  const state = join(TMP, 'state.json');
  const R1_ENV = {
    RELAYER_KEY: keyOf(2),
    CORS_ORIGIN: SITE,
    TRUST_PROXY_HEADER: 'cf-connecting-ip',
    LISTINGS: '1',
    RESERVE_PER_IP_PER_HOUR: '2',
    RESERVE_PER_MINUTE: '100',
    RESERVE_PER_HOUR: '100',
    MAX_OPEN_RESERVATIONS: '4',
    CLAIM_PER_IP_PER_HOUR: '3',
    STATE_FILE: state,
  };

  before(async () => {
    chain = await startChain();
    r1 = await startRelayer(chain, R1_ENV);
  });
  after(async () => {
    await r1?.stop();
    chain?.stop();
  });

  test('GET /status: address, balance, open orders, listings; no key', async () => {
    const res = await fetch(`${r1.url}/status`);
    const text = await res.text();
    const s = JSON.parse(text);
    assert.equal(res.status, 200);
    assert.equal(s.relayer, acct(2).address);
    assert.equal(s.checkout.toLowerCase(), chain.checkout.toLowerCase());
    assert.deepEqual(s.listings, ['1']);
    assert.equal(s.openReservations, 0);
    assert.equal(s.maxOpenReservations, 4);
    assert.equal(s.accepting, true);
    assert.ok(BigInt(s.balanceWei) > 0n);
    assert.ok(!text.toLowerCase().includes(keyOf(2).slice(2).toLowerCase()), 'the key is not served');
  });

  test('CORS: only https://sova.io; other origins and simple POSTs are refused', async () => {
    const pre = await fetch(`${r1.url}/reserve`, { method: 'OPTIONS', headers: { origin: SITE } });
    assert.equal(pre.status, 204);
    assert.equal(pre.headers.get('access-control-allow-origin'), SITE);
    const evilGet = await fetch(`${r1.url}/status`, { headers: { origin: 'https://evil.example' } });
    assert.equal(evilGet.headers.get('access-control-allow-origin'), null);
    const evil = await post(r1.url, '/reserve', { listingId: 1, recipient: addr(1) }, { origin: 'https://evil.example', ip: '198.51.100.99' });
    assert.equal(evil.status, 403);
    const plain = await fetch(`${r1.url}/reserve`, {
      method: 'POST', headers: { 'content-type': 'text/plain', origin: SITE }, body: JSON.stringify({ listingId: 1, recipient: addr(1) }),
    });
    assert.equal(plain.status, 415);
  });

  test('request hygiene: size cap, bad JSON, bad fields, other listings, unknown paths', async () => {
    const big = await post(r1.url, '/reserve', JSON.stringify({ listingId: 1, recipient: addr(1), pad: 'x'.repeat(2000) }));
    assert.equal(big.status, 413);
    assert.equal((await post(r1.url, '/reserve', '{nope')).status, 400);
    assert.equal((await post(r1.url, '/reserve', { listingId: 1, recipient: `0x${'0'.repeat(40)}` })).status, 400);
    assert.equal((await post(r1.url, '/reserve', { listingId: 2, recipient: addr(1) })).status, 403);
    assert.equal((await fetch(`${r1.url}/admin`)).status, 404);
  });

  let first;
  test('per IP: the limit holds, a spoofed X-Forwarded-For does not reset it', async () => {
    const ip = '198.51.100.1';
    first = await post(r1.url, '/reserve', { listingId: 1, recipient: addr(1) }, { ip });
    assert.equal(first.status, 200, JSON.stringify(first.body));
    assert.equal(first.body.reservationId, '1');
    const second = await post(r1.url, '/reserve', { listingId: 1, recipient: addr(2) }, { ip });
    assert.equal(second.status, 200);
    const third = await post(r1.url, '/reserve', { listingId: 1, recipient: addr(3) }, { ip, headers: { 'x-forwarded-for': '203.0.113.50' } });
    assert.equal(third.status, 429);
    assert.match(third.headers.get('retry-after'), /^\d+$/);
    assert.match(third.body.error, /too many reservations/);
    assert.equal(third.headers.get('access-control-allow-origin'), SITE, 'the 429 is readable by the page');
  });

  test('per recipient: an open order is handed back, not duplicated', async () => {
    const again = await post(r1.url, '/reserve', { listingId: 1, recipient: addr(1) }, { ip: '198.51.100.2' });
    assert.equal(again.status, 200);
    assert.equal(again.body.reservationId, first.body.reservationId);
    assert.equal(again.body.existing, true);
    assert.equal((await getJson(`${r1.url}/status`)).openReservations, 2);
  });

  test('open-order cap: at most MAX_OPEN_RESERVATIONS unpaid orders, whatever the IPs', async () => {
    assert.equal((await post(r1.url, '/reserve', { listingId: 1, recipient: addr(3) }, { ip: '198.51.100.3' })).status, 200);
    assert.equal((await post(r1.url, '/reserve', { listingId: 1, recipient: addr(4) }, { ip: '198.51.100.4' })).status, 200);
    const full = await post(r1.url, '/reserve', { listingId: 1, recipient: addr(5) }, { ip: '198.51.100.5' });
    assert.equal(full.status, 503);
    assert.match(full.body.error, /unpaid orders open/);
    const s = await getJson(`${r1.url}/status`);
    assert.equal(s.openReservations, 4);
    assert.equal(s.accepting, false);
    assert.match(s.reason, /busy/);
  });

  test('the cap survives a restart (STATE_FILE)', async () => {
    await r1.stop();
    assert.equal(statSync(state).mode & 0o777, 0o600);
    r1 = await startRelayer(chain, R1_ENV);
    assert.equal((await getJson(`${r1.url}/status`)).openReservations, 4);
    const full = await post(r1.url, '/reserve', { listingId: 1, recipient: addr(6) }, { ip: '198.51.100.6' });
    assert.equal(full.status, 503);
  });

  test('orders stop counting once their payment window has closed', async () => {
    await chain.zmine(45); // past every deadline (window 40)
    await waitFor(async () => (await getJson(`${r1.url}/status`)).openReservations === 0, 'open orders pruned');
    const ok = await post(r1.url, '/reserve', { listingId: 1, recipient: addr(7) }, { ip: '198.51.100.7' });
    assert.equal(ok.status, 200);
  });

  test('claims: dry-run refusals cost nothing and are rate limited per IP', async () => {
    const body = { reservationId: '5', txid: 'ab'.repeat(32), vout: 0 };
    for (let i = 0; i < 3; i++) {
      const r = await post(r1.url, '/claim', body, { ip: '198.51.100.8' });
      assert.equal(r.status, 400);
      assert.match(r.body.error, /^Zcash/);
    }
    const limited = await post(r1.url, '/claim', body, { ip: '198.51.100.8' });
    assert.equal(limited.status, 429);
    assert.equal((await post(r1.url, '/claim', { reservationId: '5', txid: 'zz', vout: 0 }, { ip: '198.51.100.9' })).status, 400);
  });

  test('balance floor: an unfunded relayer refuses new orders with a clear error', async () => {
    const r = await startRelayer(chain, { RELAYER_KEY: generatePrivateKey(), CORS_ORIGIN: SITE });
    try {
      const s = await getJson(`${r.url}/status`);
      assert.equal(s.accepting, false);
      assert.equal(s.reason, 'low balance');
      const res = await post(r.url, '/reserve', { listingId: 1, recipient: addr(8) });
      assert.equal(res.status, 503);
      assert.match(res.body.error, /low on SOVA/);
    } finally {
      await r.stop();
    }
  });

  test('slow blocks: POST answers 202 with the tx hash; the order still counts', async () => {
    const r = await startRelayer(chain, { RELAYER_KEY: keyOf(3), CORS_ORIGIN: SITE, RESPOND_WAIT_MS: '300' });
    await chain.pub.request({ method: 'evm_setAutomine', params: [false] });
    try {
      const res = await post(r.url, '/reserve', { listingId: 1, recipient: addr(9) });
      assert.equal(res.status, 202, JSON.stringify(res.body));
      assert.equal(res.body.pending, true);
      assert.match(res.body.txHash, /^0x[0-9a-f]{64}$/);
      const again = await post(r.url, '/reserve', { listingId: 1, recipient: addr(9) });
      assert.equal(again.status, 202);
      assert.equal(again.body.existing, true, 'a double click gets the same order');
      assert.equal(again.body.txHash, res.body.txHash);
      assert.equal((await getJson(`${r.url}/status`)).openReservations, 1);
      await chain.pub.request({ method: 'evm_mine', params: [] });
      const rc = await chain.pub.waitForTransactionReceipt({ hash: res.body.txHash });
      assert.equal(rc.status, 'success');
      await waitFor(async () => (await post(r.url, '/reserve', { listingId: 1, recipient: addr(9) })).status === 200, 'order settled');
      assert.equal((await getJson(`${r.url}/status`)).openReservations, 1);
    } finally {
      await chain.pub.request({ method: 'evm_setAutomine', params: [true] });
      await r.stop();
    }
  });

  test('keygen: creates a 0600 key file once, prints only the address', () => {
    const f = join(TMP, 'key.env');
    const a1 = execFileSync(process.execPath, [join(PKG, 'src/keygen.mjs'), f], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
    const a2 = execFileSync(process.execPath, [join(PKG, 'src/keygen.mjs'), f], { encoding: 'utf8' }).trim();
    assert.match(a1, /^0x[0-9a-fA-F]{40}$/);
    assert.equal(a1, a2, 'a second run keeps the key');
    assert.equal(statSync(f).mode & 0o777, 0o600);
    const key = readFileSync(f, 'utf8').match(/^RELAYER_KEY=(0x[0-9a-f]{64})$/m)[1];
    assert.equal(privateKeyToAccount(key).address, a1);
  });

  test('sweep: moves the relayer balance out, less the gas', async () => {
    const to = addr(99);
    execFileSync(process.execPath, [join(PKG, 'src/sweep.mjs'), to], {
      env: { PATH: process.env.PATH, RELAYER_KEY: keyOf(4), SOVA_RPC_URL: chain.rpc }, encoding: 'utf8',
    });
    const left = await chain.pub.getBalance({ address: acct(4).address });
    assert.ok(left < 10n ** 15n, `left ${left}`);
    assert.ok((await chain.pub.getBalance({ address: to })) > 9999n * 10n ** 18n);
  });
});

if (skip) test(`chain tests skipped: ${skip}`, { skip: true }, () => {});
