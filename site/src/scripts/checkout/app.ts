// /ashwings/buy: reserve an Ashwing, pay ZEC, claim once Sova sees it.
// Config comes from the page's data-config, overridden by query params:
//   ?rpc=<sova rpc>&co=<checkout>&listing=<id>&relayer=<url|none>&net=test|main
// The order id and txid live in the URL (?r=&tx=) so a reload resumes.
import { qrSvg } from './qr';
import {
  ZCASH, ZERO_ADDR, SEL, TOPIC, rpc, reason, u256, addrWord, b32, calldata, words, num, asAddr,
  dynBytes, utf8, payeeScript, tAddr, zec,
} from './chain';

type Cfg = { rpc: string; checkout: string; listing: number; relayer: string; net: 'test' | 'main' };
type Resv = {
  recipient: string; quote: bigint; minConf: number; p2sh: boolean; filled: boolean;
  hash: string; reservedAt: bigint; window: bigint;
};

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const root = $('buy');
const q = new URLSearchParams(location.search);
const base = JSON.parse(root.dataset.config || '{}') as Cfg;
const cfg: Cfg = {
  rpc: q.get('rpc') || base.rpc,
  checkout: q.get('co') || base.checkout,
  listing: Number(q.get('listing') || base.listing),
  relayer: q.get('relayer') === 'none' ? '' : (q.get('relayer') || base.relayer).replace(/\/$/, ''),
  net: (q.get('net') as Cfg['net']) || base.net,
};
const eth = (window as any).ethereum as { request(a: { method: string; params?: unknown[] }): Promise<any> } | undefined;

const S = {
  rid: q.get('r') ? BigInt(q.get('r')!) : 0n,
  txid: (q.get('tx') || '').toLowerCase(),
  vout: -1,
  wallet: '',
  resv: null as Resv | null,
  shownFor: '',
  busy: false,
  owl: false,
};

// ---- chain reads ----------------------------------------------------------

const call = (to: string, data: string, from?: string) =>
  rpc<string>(cfg.rpc, 'eth_call', [{ to, data, ...(from ? { from } : {}) }, 'latest']);

async function reservation(id: bigint): Promise<Resv> {
  const w = words(await call(cfg.checkout, calldata(SEL.reservations, u256(id))));
  return {
    recipient: asAddr(w[0]), quote: num(w[1]), minConf: Number(num(w[2])), p2sh: num(w[3]) === 1n,
    filled: num(w[4]) === 1n, hash: w[5].slice(0, 40), reservedAt: num(w[6]), window: num(w[7]),
  };
}
const anchorHeight = async () => num(words(await call(ZCASH, '0x' + SEL.anchor))[0]);

async function txInfo(txid: string) {
  const w = words(await call(ZCASH, calldata(SEL.txInfo, b32(txid))));
  return { status: Number(num(w[0])), height: num(w[1]), conf: num(w[3]), nOut: Number(num(w[4])) };
}
async function txOutput(txid: string, vout: number) {
  const ret = await call(ZCASH, calldata(SEL.txOutput, b32(txid), u256(vout)));
  return { status: Number(num(words(ret)[0])), value: num(words(ret)[1]), script: dynBytes(ret, 2) };
}

/** Item id and paying txid for a filled order, from its Claimed event. */
async function claimed(id: bigint): Promise<{ item: bigint; txid: string } | null> {
  const logs = await rpc<any[]>(cfg.rpc, 'eth_getLogs', [
    { address: cfg.checkout, topics: [TOPIC.claimed, '0x' + u256(id)], fromBlock: '0x0', toBlock: 'latest' },
  ]);
  if (!logs.length) return null;
  const w = words(logs[0].data);
  return { item: num(w[3]), txid: w[0] };
}

// ---- sending: injected wallet, else relayer --------------------------------

async function receipt(hash: string) {
  for (let i = 0; i < 120; i++) {
    const r = await rpc<any>(cfg.rpc, 'eth_getTransactionReceipt', [hash]);
    if (r) {
      if (r.status !== '0x1') throw new Error('transaction reverted');
      return r;
    }
    await new Promise((ok) => setTimeout(ok, 1500));
  }
  throw new Error('no receipt after 3 min');
}

async function walletSend(data: string) {
  const chain = await rpc<string>(cfg.rpc, 'eth_chainId');
  if ((await eth!.request({ method: 'eth_chainId' })) !== chain) {
    await eth!.request({ method: 'wallet_switchEthereumChain', params: [{ chainId: chain }] });
  }
  const hash = await eth!.request({ method: 'eth_sendTransaction', params: [{ from: S.wallet, to: cfg.checkout, data }] });
  return receipt(hash);
}

async function relay(path: string, body: unknown) {
  if (!cfg.relayer) throw new Error('no relayer configured: connect a wallet');
  const res = await fetch(cfg.relayer + path, {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
  });
  const j = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(j.error || `relayer ${res.status}`);
  return j;
}

// ---- UI -------------------------------------------------------------------

const STEPS = ['reserve', 'pay', 'conf', 'claim', 'owl'];
function stage(k: string, conf?: string) {
  const at = STEPS.indexOf(k);
  document.querySelectorAll<HTMLElement>('.track li').forEach((li, i) => {
    li.dataset.s = i < at ? 'done' : i === at ? 'cur' : 'todo';
  });
  if (conf) $('trk-conf').textContent = conf;
}
function status(kind: 'wait' | 'ok' | 'err', text: string) {
  const el = $('status');
  el.dataset.k = kind;
  $('st').textContent = text;
}
const short = (a: string) => a.slice(0, 8) + '…' + a.slice(-6);
function setUrl() {
  const u = new URL(location.href);
  S.rid ? u.searchParams.set('r', String(S.rid)) : u.searchParams.delete('r');
  S.txid ? u.searchParams.set('tx', S.txid) : u.searchParams.delete('tx');
  history.replaceState(null, '', u);
}

async function showPay(r: Resv) {
  const key = `${S.rid}:${r.quote}`;
  if (S.shownFor === key) return;
  S.shownFor = key;
  const addr = await tAddr(r.hash, r.p2sh, cfg.net);
  const amount = zec(r.quote);
  const uri = `zcash:${addr}?amount=${amount}`;
  $('qr').innerHTML = qrSvg(uri);
  $('qr').setAttribute('aria-label', `QR code for ${uri}`);
  $('amt').textContent = amount;
  $('taddr').textContent = addr;
  $('uri').textContent = uri;
  $('uri').setAttribute('href', uri); // tap to open a phone wallet
  $('rid').textContent = String(S.rid);
  $('to').textContent = short(r.recipient);
  $('s2').hidden = false;
  $('s3').hidden = false;
  $<HTMLInputElement>('addr').value = r.recipient;
  $('s1').classList.add('done');
}

async function showOwl(itemId: bigint) {
  if (S.owl) return;
  const ashw = asAddr(words(await call(cfg.checkout, '0x' + SEL.ashwings))[0]);
  const uri = utf8(dynBytes(await call(ashw, calldata(SEL.tokenURI, u256(itemId))), 0));
  const meta = JSON.parse(atob(uri.split(',')[1]));
  const img = $<HTMLImageElement>('owl-img');
  img.src = meta.image;
  img.alt = meta.name;
  $('owl-name').textContent = meta.name;
  $('owl').hidden = false;
  S.owl = true;
}

// ---- the loop ---------------------------------------------------------------

async function tick() {
  if (!S.rid || S.busy) return;
  S.busy = true;
  try {
    await step();
  } catch (e) {
    status('err', reason(e));
  } finally {
    S.busy = false;
  }
}

async function step() {
  const r = await reservation(S.rid);
  if (r.recipient === ZERO_ADDR) return status('err', `no order #${S.rid} on this checkout`);
  S.resv = r;
  await showPay(r);
  const claimBtn = $<HTMLButtonElement>('claim');
  claimBtn.disabled = true;

  if (r.filled) {
    const c = await claimed(S.rid);
    stage('owl', `${r.minConf}/${r.minConf} conf`);
    if (!c) return status('ok', 'minted');
    if (!S.txid) {
      S.txid = c.txid;
      $<HTMLInputElement>('txid').value = c.txid;
      setUrl();
    }
    await showOwl(c.item);
    $('s1').hidden = true;
    $('s3').hidden = true;
    $('s2h').textContent = 'paid';
    $('s2').classList.add('paid');
    return status('ok', `minted · Ashwing #${c.item} → ${short(r.recipient)}`);
  }

  const anchor = await anchorHeight();
  const dl = r.reservedAt + r.window;
  $('dl').textContent = `${dl}`;
  $('dl-min').textContent = anchor < dl ? `~${Math.ceil(Number(dl - anchor) * 75 / 60)} min left` : 'closed';

  // A relayer that watches the seller's t-address may have found it for us.
  if (!S.txid && cfg.relayer) {
    const j = await fetch(`${cfg.relayer}/status/${S.rid}`).then((x) => x.json()).catch(() => null);
    if (j?.detected?.txid) {
      S.txid = j.detected.txid;
      S.vout = j.detected.vout;
      $<HTMLInputElement>('txid').value = S.txid;
      setUrl();
    }
  }

  if (!S.txid) {
    stage('pay', `0/${r.minConf} conf`);
    if (anchor > dl) return status('err', `window closed at zcash block ${dl}: this quote expired, reserve again`);
    return status('wait', `waiting for payment · zcash anchor ${anchor}`);
  }

  const info = await txInfo(S.txid);
  if (info.status !== 0) {
    stage('pay', `0/${r.minConf} conf`);
    return status('wait', `tx sent? not on Sova's zcash view yet · anchor ${anchor}`);
  }
  const script = payeeScript(r.hash, r.p2sh);
  if (S.vout < 0) {
    for (let v = 0; v < info.nOut && S.vout < 0; v++) {
      const o = await txOutput(S.txid, v);
      if (o.status === 0 && o.script === script && o.value === r.quote) S.vout = v;
    }
    if (S.vout < 0) return status('err', `that tx has no output of exactly ${zec(r.quote)} ZEC to the seller`);
  }
  if (info.height <= r.reservedAt) return status('err', 'that payment was mined before this order');
  if (info.height > dl) return status('err', `mined at ${info.height}, after the deadline ${dl}: not claimable`);

  if (info.conf < BigInt(r.minConf)) {
    stage('conf', `${info.conf}/${r.minConf} conf`);
    return status('wait', `seen in zcash block ${info.height} · ${info.conf}/${r.minConf} confirmations`);
  }

  // Deep enough: dry-run the claim so the button only lights when it will work.
  try {
    await call(cfg.checkout, claimData(), S.wallet || undefined);
  } catch (e) {
    return status('err', reason(e));
  }
  stage('claim', `${r.minConf}/${r.minConf} conf`);
  claimBtn.disabled = false;
  status('ok', `claimable · ${info.conf} confirmations`);
}

const claimData = () => calldata(SEL.claim, u256(S.rid), b32(S.txid), u256(S.vout));

// ---- actions ----------------------------------------------------------------

async function doReserve() {
  const addr = $<HTMLInputElement>('addr').value.trim();
  if (!/^0x[0-9a-fA-F]{40}$/.test(addr) || /^0x0{40}$/.test(addr)) return status('err', 'enter a Sova (EVM) address: 0x + 40 hex');
  const btn = $<HTMLButtonElement>('reserve');
  btn.disabled = true;
  status('wait', S.wallet ? 'reserving · confirm in your wallet' : 'reserving · relayer pays the gas');
  try {
    if (S.wallet) {
      const rc = await walletSend(calldata(SEL.reserve, u256(cfg.listing), addrWord(addr)));
      const log = rc.logs.find((l: any) => l.topics[0] === TOPIC.reserved);
      S.rid = num(log.topics[1].slice(2));
    } else {
      const j = await relay('/reserve', { listingId: cfg.listing, recipient: addr });
      S.rid = BigInt(j.reservationId);
    }
    S.txid = '';
    S.vout = -1;
    setUrl();
    await tick();
  } catch (e) {
    status('err', reason(e));
  } finally {
    btn.disabled = false;
  }
}

async function doClaim() {
  const btn = $<HTMLButtonElement>('claim');
  btn.disabled = true;
  status('wait', S.wallet ? 'claiming · confirm in your wallet' : 'claiming · relayer pays the gas');
  try {
    if (S.wallet) await walletSend(claimData());
    else await relay('/claim', { reservationId: String(S.rid), txid: S.txid, vout: S.vout });
    await tick();
  } catch (e) {
    btn.disabled = false;
    status('err', reason(e));
  }
}

function setTxid() {
  const t = $<HTMLInputElement>('txid').value.trim().replace(/^0x/, '').toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(t)) return status('err', 'a txid is 64 hex characters');
  S.txid = t;
  S.vout = -1;
  setUrl();
  tick();
}

async function connect() {
  try {
    const [a] = await eth!.request({ method: 'eth_requestAccounts' });
    S.wallet = a;
    $<HTMLInputElement>('addr').value = a;
    $('connect').textContent = 'wallet ' + short(a);
  } catch (e) {
    status('err', reason(e));
  }
}

// ---- boot -------------------------------------------------------------------

$('reserve').addEventListener('click', doReserve);
$('claim').addEventListener('click', doClaim);
$('check').addEventListener('click', setTxid);
$('txid').addEventListener('keydown', (e) => e.key === 'Enter' && setTxid());
if (eth) {
  $('connect').hidden = false;
  $('connect').addEventListener('click', connect);
}
document.querySelectorAll<HTMLButtonElement>('[data-copy]').forEach((b) =>
  b.addEventListener('click', async () => {
    await navigator.clipboard.writeText($(b.dataset.copy!).textContent || '').catch(() => {});
    const t = b.textContent;
    b.textContent = 'copied';
    setTimeout(() => (b.textContent = t), 1200);
  }),
);
$('cfg').textContent = `rpc ${cfg.rpc} · checkout ${short(cfg.checkout)} · listing ${cfg.listing} · relayer ${cfg.relayer || 'none'} · ${cfg.net}net`;
if (S.txid) $<HTMLInputElement>('txid').value = S.txid;
stage('reserve', '0/? conf');
status('wait', 'enter your address and reserve');
tick();
setInterval(tick, 3000);
