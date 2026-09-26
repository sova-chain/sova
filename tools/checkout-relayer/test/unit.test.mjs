// Pure pieces of the abuse limits: no chain needed.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { clientKey, throttledFetch, windows } from '../src/limits.mjs';

const req = (peer, headers = {}) => ({ socket: { remoteAddress: peer }, headers });

test('clientKey: the proxy header counts only when the peer is loopback', () => {
  assert.equal(clientKey(req('127.0.0.1', { 'cf-connecting-ip': '198.51.100.7' }), 'cf-connecting-ip'), '198.51.100.7');
  assert.equal(clientKey(req('::ffff:127.0.0.1', { 'cf-connecting-ip': '198.51.100.7' }), 'cf-connecting-ip'), '198.51.100.7');
  // A direct caller can't pick its bucket.
  assert.equal(clientKey(req('203.0.113.9', { 'cf-connecting-ip': '198.51.100.7' }), 'cf-connecting-ip'), '203.0.113.9');
  // No header configured: the socket peer.
  assert.equal(clientKey(req('127.0.0.1', { 'cf-connecting-ip': '198.51.100.7' }), ''), '127.0.0.1');
  // Garbage in the header: the socket peer.
  assert.equal(clientKey(req('127.0.0.1', { 'cf-connecting-ip': 'nope' }), 'cf-connecting-ip'), '127.0.0.1');
  assert.equal(clientKey(req('::ffff:203.0.113.9')), '203.0.113.9');
});

test('clientKey: X-Forwarded-For uses the rightmost entry (the one our proxy appended)', () => {
  const r = req('127.0.0.1', { 'x-forwarded-for': '10.9.9.9, 1.2.3.4, 198.51.100.8' });
  assert.equal(clientKey(r, 'x-forwarded-for'), '198.51.100.8');
});

test('clientKey: IPv6 is keyed per /64', () => {
  const a = clientKey(req('127.0.0.1', { 'cf-connecting-ip': '2001:db8:1:2:aaaa::1' }), 'cf-connecting-ip');
  const b = clientKey(req('127.0.0.1', { 'cf-connecting-ip': '2001:db8:1:2:bbbb:cccc:dddd:eeee' }), 'cf-connecting-ip');
  const c = clientKey(req('127.0.0.1', { 'cf-connecting-ip': '2001:db8:1:3::1' }), 'cf-connecting-ip');
  assert.equal(a, '2001:db8:1:2::/64');
  assert.equal(a, b);
  assert.notEqual(a, c);
  assert.equal(clientKey(req('::1')), '0:0:0:0::/64');
});

test('windows: fixed window per key, resets after the period', () => {
  let t = 0;
  const w = windows(() => t);
  assert.ok(w.allow('a', 2, 1000));
  assert.ok(w.allow('a', 2, 1000));
  assert.ok(!w.allow('a', 2, 1000));
  assert.ok(w.allow('b', 2, 1000), 'another key has its own window');
  assert.equal(w.retryAfter('a'), 1);
  t = 1000;
  assert.ok(w.allow('a', 2, 1000), 'new window');
  assert.ok(!w.allow('z', 0, 1000), 'limit 0 refuses everything');
  w.stop();
});

test('throttledFetch: at most N requests per period, in order', async () => {
  const at = [];
  const f = throttledFetch(3, 300, async (u) => {
    at.push([u, Date.now()]);
    return u;
  });
  const t0 = Date.now();
  const got = await Promise.all([1, 2, 3, 4, 5, 6, 7].map((i) => f(i)));
  assert.deepEqual(got, [1, 2, 3, 4, 5, 6, 7]);
  assert.deepEqual(at.map((x) => x[0]), [1, 2, 3, 4, 5, 6, 7]);
  // 7 requests at 3 per 300 ms need at least two full periods.
  assert.ok(Date.now() - t0 >= 590, `took ${Date.now() - t0} ms`);
  for (let i = 3; i < at.length; i++) assert.ok(at[i][1] - at[i - 3][1] >= 295, `request ${i + 1} too early`);
});
