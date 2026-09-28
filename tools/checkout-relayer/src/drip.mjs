// The testnet SOVA drip's bookkeeping (POST /drip in server.mjs): who got
// SOVA and when, so the limits hold across restarts. A drip tops a new
// address up to one Ashwings mint plus gas, so a stranger with a browser
// wallet can mint a first owl without mining first.
//
// Limits, like the TAZ faucet's (crates/burn-wallet/faucet): one drip per
// address per cooldown, a few per client IP per day, and hourly and daily
// caps for everyone. Addresses and times are kept in the relayer's
// STATE_FILE; client IPs are kept in memory only (never written to disk).

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

export class DripBook {
  /**
   * @param {{ addressCooldownMs: number, perIpPerDay: number, perHour: number, perDay: number, now?: () => number }} o
   */
  constructor({ addressCooldownMs, perIpPerDay, perHour, perDay, now = Date.now }) {
    Object.assign(this, { addressCooldownMs, perIpPerDay, perHour, perDay, now });
    /** @type {{ addr: string, at: number, txHash?: string }[]} oldest first */
    this.log = [];
    /** @type {Map<string, number[]>} client IP -> drip times (memory only) */
    this.byIp = new Map();
  }

  /** Restores the persisted log (addresses and times; no IPs). */
  load(list) {
    this.log = (Array.isArray(list) ? list : [])
      .filter((d) => d && typeof d.addr === 'string' && Number.isFinite(d.at))
      .map((d) => ({ addr: d.addr.toLowerCase(), at: d.at, ...(d.txHash ? { txHash: d.txHash } : {}) }))
      .sort((a, b) => a.at - b.at);
    this.prune();
  }

  toJSON() {
    this.prune();
    return this.log;
  }

  prune() {
    const keep = this.now() - Math.max(this.addressCooldownMs, DAY);
    while (this.log.length && this.log[0].at < keep) this.log.shift();
    const day = this.now() - DAY;
    for (const [ip, ts] of this.byIp) {
      const live = ts.filter((t) => t >= day);
      if (live.length) this.byIp.set(ip, live);
      else this.byIp.delete(ip);
    }
  }

  /** Drips in the last `ms`. */
  since(ms) {
    const t = this.now() - ms;
    let n = 0;
    for (let i = this.log.length - 1; i >= 0 && this.log[i].at >= t; i--) n++;
    return n;
  }

  /**
   * Null if `addr` from `ip` may get a drip now, else why not:
   * { code, error, retryAfter } (seconds). Counts nothing.
   */
  check(addr, ip) {
    this.prune();
    const a = addr.toLowerCase();
    const t = this.now();
    const secs = (until) => Math.max(1, Math.ceil((until - t) / 1000));
    const last = this.log.findLast((d) => d.addr === a);
    if (last && t - last.at < this.addressCooldownMs) {
      return { code: 'address_cooldown', error: 'this address already got SOVA today', retryAfter: secs(last.at + this.addressCooldownMs) };
    }
    const mine = this.byIp.get(ip) || [];
    if (mine.length >= this.perIpPerDay) {
      return { code: 'ip_cooldown', error: 'your network already got SOVA today', retryAfter: secs(mine[0] + DAY) };
    }
    if (this.since(HOUR) >= this.perHour) {
      const first = this.log[this.log.length - this.perHour];
      return { code: 'hourly_cap', error: 'the drip is busy: try again within the hour', retryAfter: secs(first.at + HOUR) };
    }
    if (this.since(DAY) >= this.perDay) {
      const first = this.log[this.log.length - this.perDay];
      return { code: 'daily_cap', error: 'the drip is out for today: try tomorrow', retryAfter: secs(first.at + DAY) };
    }
    return null;
  }

  /** Counts a drip (before sending, so a double click can't pass twice). */
  record(addr, ip) {
    const entry = { addr: addr.toLowerCase(), at: this.now() };
    this.log.push(entry);
    this.byIp.set(ip, [...(this.byIp.get(ip) || []), entry.at]);
    return entry;
  }

  /** Gives the slot back: the send failed, nothing went out. */
  forget(entry, ip) {
    const i = this.log.lastIndexOf(entry);
    if (i >= 0) this.log.splice(i, 1);
    const ts = this.byIp.get(ip) || [];
    const j = ts.lastIndexOf(entry.at);
    if (j >= 0) ts.splice(j, 1);
  }
}
