// Homepage testnet stats (components/TestnetStats.astro), read once per page
// view from the public RPC (rate-limited per IP, so no polling). Two calls:
// eth_getBlockByNumber('latest') for the Sova height and that block's time,
// and ZcashBlocks.latest() (SIP-7, as /pulse reads it) for the newest
// anchored Zcash height. A cell whose call fails keeps its server-rendered
// placeholder, which is also what shows with no JS. ?rpc= overrides the RPC.
import { rpc } from './checkout/chain';
import { latestAnchored } from './zcash-blocks';

type Block = { number: string; timestamp: string };

const grp = (n: number) => String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ',');
const ago = (s: number) =>
  s < 120 ? `${s} s ago` : s < 7200 ? `${Math.floor(s / 60)} min ago` : `${Math.floor(s / 3600)} h ago`;

async function main() {
  const boxes = [...document.querySelectorAll<HTMLElement>('.ts[data-rpc]')];
  if (!boxes.length) return;
  const url = new URLSearchParams(location.search).get('rpc') || boxes[0].dataset.rpc!;
  const host = (() => {
    try { return new URL(url).host; } catch { return url; }
  })();

  const [blk, zh] = await Promise.allSettled([
    rpc<Block>(url, 'eth_getBlockByNumber', ['latest', false]),
    latestAnchored(url),
  ]);

  const set = (key: string, text: string) => {
    for (const b of boxes) {
      const dd = b.querySelector<HTMLElement>(`[data-stat="${key}"]`);
      if (dd) dd.textContent = text;
    }
  };
  let ok = false;
  if (blk.status === 'fulfilled' && blk.value) {
    ok = true;
    set('sova', grp(Number(BigInt(blk.value.number))));
    const t = Number(BigInt(blk.value.timestamp));
    const age = () => set('age', ago(Math.max(0, Math.floor(Date.now() / 1000) - t)));
    age();
    setInterval(age, 1000); // local clock only, no further RPC
  }
  if (zh.status === 'fulfilled' && zh.value > 0) {
    ok = true;
    set('zcash', grp(zh.value));
  }
  for (const b of boxes) {
    b.dataset.live = ok ? '1' : '0';
    const note = b.querySelector('.ts-note');
    if (note) note.textContent = ok ? `read live from ${host}` : `can't reach ${host} right now`;
  }
}

main();
