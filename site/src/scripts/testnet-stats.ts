// Homepage testnet stats (components/TestnetStats.astro), read from the public
// RPC when the page loads and again every POLL_MS while the tab is visible, so
// a new block shows up without a reload. Two calls per read:
// eth_getBlockByNumber('latest') for the Sova height and that block's time,
// and ZcashBlocks.latest() (SIP-7, as /pulse reads it) for the newest
// anchored Zcash height. That is 12 requests a minute per open tab, far under
// the RPC's per-IP limit (50 per 10 s). A cell whose read fails keeps its last
// value (or the server-rendered placeholder, which is also what shows with no
// JS). A cell that changes flashes once. ?rpc= overrides the RPC.
import { rpc } from './checkout/chain';
import { latestAnchored } from './zcash-blocks';

type Block = { number: string; timestamp: string };

const POLL_MS = 10_000;

const grp = (n: number) => String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
const ago = (s: number) =>
  s < 120 ? `${s} s ago` : s < 7200 ? `${Math.floor(s / 60)} min ago` : `${Math.floor(s / 3600)} h ago`;

function main() {
  const boxes = [...document.querySelectorAll<HTMLElement>('.ts[data-rpc]')];
  if (!boxes.length) return;
  const url = new URLSearchParams(location.search).get('rpc') || boxes[0].dataset.rpc!;
  const host = (() => {
    try { return new URL(url).host; } catch { return url; }
  })();

  const shown: Record<string, string> = {};
  const set = (key: string, text: string, flash = false) => {
    if (shown[key] === text) return;
    const changed = key in shown;
    shown[key] = text;
    for (const b of boxes) {
      const dd = b.querySelector<HTMLElement>(`[data-stat="${key}"]`);
      if (!dd) continue;
      dd.textContent = text;
      if (flash && changed) {
        dd.classList.remove('ts-new');
        void dd.offsetWidth; // restart the animation
        dd.classList.add('ts-new');
      }
    }
  };

  let blockTime = 0;
  let everOk = false;
  setInterval(() => {
    if (blockTime) set('age', ago(Math.max(0, Math.floor(Date.now() / 1000) - blockTime)));
  }, 1000); // local clock only, no RPC

  let busy = false;
  async function read() {
    if (busy || document.hidden) return;
    busy = true;
    try {
      const [blk, zh] = await Promise.allSettled([
        rpc<Block>(url, 'eth_getBlockByNumber', ['latest', false]),
        latestAnchored(url),
      ]);
      let ok = false;
      if (blk.status === 'fulfilled' && blk.value) {
        ok = true;
        set('sova', grp(Number(BigInt(blk.value.number))), true);
        blockTime = Number(BigInt(blk.value.timestamp));
        set('age', ago(Math.max(0, Math.floor(Date.now() / 1000) - blockTime)));
      }
      if (zh.status === 'fulfilled' && zh.value > 0) {
        ok = true;
        set('zcash', grp(zh.value), true);
      }
      everOk ||= ok;
      for (const b of boxes) {
        b.dataset.live = ok ? '1' : '0';
        const note = b.querySelector('.ts-note');
        if (note) {
          note.textContent = ok
            ? `read live from ${host}`
            : everOk ? `lost ${host}; retrying` : `can't reach ${host} right now`;
        }
      }
    } finally {
      busy = false;
    }
  }

  read();
  setInterval(read, POLL_MS);
  // A tab in the background doesn't poll; catch up as soon as it's back.
  document.addEventListener('visibilitychange', () => { if (!document.hidden) read(); });
}

main();
