// Abuse limits for a public relayer: fixed-window counters, the client key
// a request is counted under, and a throttle for the relayer's own RPC use.
import { isIP } from 'node:net';

/** Fixed-window counters: key -> { n, reset }. */
export function windows(now = Date.now) {
  const w = new Map();
  const sweep = setInterval(() => {
    const t = now();
    for (const [k, v] of w) if (t >= v.reset) w.delete(k);
  }, 60_000);
  sweep.unref();
  return {
    /** Counts one hit; false once `limit` hits are in the current window. */
    allow(key, limit, ms) {
      const t = now();
      const v = w.get(key);
      if (!v || t >= v.reset) {
        w.set(key, { n: 1, reset: t + ms });
        return limit >= 1;
      }
      return ++v.n <= limit;
    },
    /** Seconds until `key`'s window resets (for Retry-After). */
    retryAfter(key) {
      const v = w.get(key);
      return v ? Math.max(1, Math.ceil((v.reset - now()) / 1000)) : 1;
    },
    stop: () => clearInterval(sweep),
  };
}

const LOOPBACK = new Set(['127.0.0.1', '::1', '::ffff:127.0.0.1']);

/**
 * The key a request is rate-limited under. Behind our own proxy
 * (cloudflared on this host) every socket peer is loopback, so the client
 * IP comes from the proxy's header, but only when the peer really is
 * loopback: a direct caller can't pick its own bucket. `header` is
 * 'cf-connecting-ip' (Cloudflare overwrites it; the client can't forge it
 * through the edge) or 'x-forwarded-for' (the RIGHTMOST entry, the one our
 * proxy appended). IPv6 is keyed per /64: one host gets a whole /64.
 */
export function clientKey(req, header) {
  const peer = req.socket.remoteAddress || '';
  let ip = peer;
  if (header && LOOPBACK.has(peer)) {
    const raw = req.headers[header];
    const v = String(Array.isArray(raw) ? raw[raw.length - 1] : raw || '')
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean)
      .pop();
    if (v && isIP(v)) ip = v;
  }
  if (ip.startsWith('::ffff:') && isIP(ip.slice(7)) === 4) ip = ip.slice(7);
  if (isIP(ip) === 6) return ip6Prefix64(ip);
  return ip;
}

function ip6Prefix64(ip) {
  // Expand "::" and keep the first four groups.
  const [head, tail = ''] = ip.split('::');
  const h = head ? head.split(':') : [];
  const t = tail ? tail.split(':') : [];
  const groups = ip.includes('::') ? [...h, ...Array(8 - h.length - t.length).fill('0'), ...t] : h;
  return groups.slice(0, 4).map((g) => (parseInt(g, 16) || 0).toString(16)).join(':') + '::/64';
}

/**
 * fetch() wrapper that sends at most `max` requests per `ms`. The public
 * RPC limits each IP (the zone's WAF rule, 50 per 10 s); everything the
 * relayer does on Sova goes through here, so it stays under that and
 * queues instead of getting its own IP blocked.
 */
export function throttledFetch(max, ms = 10_000, inner = fetch) {
  const sent = [];
  let chain = Promise.resolve();
  const slot = async () => {
    for (;;) {
      const t = Date.now();
      while (sent.length && t - sent[0] >= ms) sent.shift();
      if (sent.length < max) {
        sent.push(t);
        return;
      }
      await new Promise((r) => setTimeout(r, ms - (t - sent[0]) + 5));
    }
  };
  return (url, init) => {
    const turn = chain.then(slot);
    chain = turn.catch(() => {});
    return turn.then(() => inner(url, init));
  };
}
