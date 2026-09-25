// Shared facts for the /v/* design directions. Every value here is sourced in
// site/README.md ("Where each claim comes from"); the notes below say which
// file each block comes from. The three direction pages render these, so a
// fact changes in one place.

import testnetDeployments from '../../../infra/testnet/deployments/sova-testnet.json';

// The Sova RPC the live pages (/pulse, /ashwings/*) read by default: the
// public testnet's RPC (docs/ops/testnet-launch.md, B5 f; read-only, rate
// limited per IP). Each page still takes ?rpc= per visit, e.g.
// ?rpc=http://127.0.0.1:8545 for the local box.
export const SOVA_RPC = 'https://rpc.testnet.sova.io';

// The public testnet (docs/ops/testnet-launch.md; seeds.json `courtesy`;
// docs/guides/testnet.md). Project-run conveniences, never load-bearing.
// No block explorer yet.
export const TESTNET = {
  chainId: testnetDeployments.chainId, // 82330
  rpc: SOVA_RPC,
  faucet: 'https://faucet.testnet.sova.io',
  downloads: 'https://dl.testnet.sova.io',
  guide: 'docs/guides/testnet.md',
} as const;

// Day-one contracts on the public testnet, straight from the deploy record
// (infra/testnet/deployments/sova-testnet.json, written by
// infra/testnet/deploy-contracts.sh). The Ashwings ZEC checkout isn't listed
// there: the Ashwings constructor creates it, so /ashwings/buy reads
// Ashwings.zecCheckout() at load.
export const CONTRACTS = {
  wsova: testnetDeployments.wsova,
  factory: testnetDeployments.factory,
  router: testnetDeployments.router,
  multicall3: testnetDeployments.multicall3,
  ashwings: testnetDeployments.ashwings,
  market: testnetDeployments.market,
} as const;

// The owner-picked positioning line (docs/marketing/positioning.md).
export const EDGE_LINE = 'The programmable edge of the shielded pool.';

// Constants from sips/sip-3.md (Constants) and sips/sip-1.md (dust floor).
export const REWARD_SOVA = '6,250';
export const HALVING_EPOCHS = '1,680,000';
export const SLOW_START_EPOCHS = '20,000';
export const MIN_BURN_ZAT = '1,000';
export const BLOCK_SECONDS = 75; // Zcash's target spacing; one epoch per Zcash block

// Regtest transcript, shortened. Values from mcp/docs/walkthrough.md (epoch 1)
// and box/up/README.md (node log line, balance hex); formats are the tools' own
// (crates/burn-wallet/miner/src/{main,mine}.rs, crates/engine/src/driver.rs).
export type Line = { k: 'cmd' | 'out' | 'log' | 'note' | 'mint'; t: string };
export const transcript: Line[] = [
  { k: 'cmd', t: 'sova-miner --network regtest mine --per-epoch-zat 100000' },
  { k: 'out', t: 'epoch 1: height=103 burn=100000zat txid=c5632d5e…' },
  { k: 'note', t: '100000 zat to 76a914 00…00 88ac: unspendable' },
  { k: 'log', t: 'INFO sova epoch trigger height=103 settled=true' },
  { k: 'cmd', t: 'curl -s localhost:8545 -d \'{…"eth_getBalance"…}\'' },
  { k: 'out', t: '{"result":"0x152d02c7e14af680000"}' },
  { k: 'mint', t: '0x152d02c7e14af680000 wei = 6,250 SOVA' },
];

// What we never claim (positioning.md, "What we never claim"), one line each.
export const notClaims = [
  ['Not a privacy chain', 'Execution is public. Privacy is how you fund your way in.'],
  ['No private smart contracts', 'Every call and state change is visible.'],
  ['Not a Zcash L2', 'A sidechain with its own consensus, anchored by burns.'],
  ['No peg in the protocol', 'ZEC stays on Zcash. Wrapped ZEC is a separate, custodial Sova Labs product (wz.cash), not part of the network.'],
  ['SOVA is gas', 'Not governance, not staking, not a claim on anything.'],
] as const;

// The public repository: github.com/sova-chain/sova, default branch `main`
// (an export of the `release` trunk, same file layout).
export const REPO = 'https://github.com/sova-chain/sova';
export const CLONE = `git clone ${REPO} && cd sova`;

// Community (Rob, 2026-09-23): the project's Telegram group and X account.
export const SOCIAL = {
  telegram: 'https://t.me/sovazec',
  x: 'https://x.com/sovazec',
} as const;
export const repo = {
  root: REPO,
  sips: `${REPO}/tree/main/sips`,
  docs: `${REPO}/tree/main/docs`,
  issues: `${REPO}/issues`,
  discussions: `${REPO}/discussions`,
  contributing: `${REPO}/blob/main/CONTRIBUTING.md`,
  securityPolicy: `${REPO}/blob/main/SECURITY.md`,
  reportVulnerability: `${REPO}/security/advisories/new`,
  /** A file on `main`, e.g. `sips/sip-1.md`. */
  blob: (path: string) => `${REPO}/blob/main/${path}`,
  /** A directory on `main`, e.g. `contracts/src/zcash`. */
  tree: (path: string) => `${REPO}/tree/main/${path}`,
};

// The SIPs (sips/*.md; SIP-5 is withdrawn, its text kept as
// sips/sip-5-withdrawn.md). `st` is the status
// word shown as a tag; `note` is the status detail; `file` is the text in
// sips/ (linked on GitHub). Rendered by /sips and the landing page.
export const sips = [
  {
    n: 1, id: 'sip-1', file: 'sip-1.md', title: 'Burn transaction format', st: 'frozen',
    sum: 'What a burn is: one transparent Zcash transaction that pays ZEC to an unspendable script and names the EVM address to credit.',
    note: 'Frozen after a burn was relayed through public Zcash testnet peers and mined by an unrelated miner (txid 641cc306…, height 4,383,754).',
  },
  {
    n: 2, id: 'sip-2', file: 'sip-2.md', title: 'Epochs, rewards and settlement', st: 'draft',
    sum: 'One Zcash block is one epoch. Burners rank by weight, the top burner seals, and rewards land as the block’s withdrawals, re-derived by every node.',
    note: 'Implemented; proven on regtest.',
  },
  {
    n: 3, id: 'sip-3', file: 'sip-3.md', title: 'Emission schedule', st: 'accepted',
    sum: '6,250 SOVA per epoch after a 20,000-epoch slow start, halving every 1,680,000 epochs, Zcash’s own interval.',
    note: 'Numbers locked; the schedule starts at mainnet. The public testnet mints a flat 6,250 SOVA per epoch.',
  },
  {
    n: 4, id: 'sip-4', file: 'sip-4-draft-zcash-state-precompile.md', title: 'Zcash state precompile', st: 'draft',
    sum: 'Contracts read transparent Zcash state, as of the Zcash block each Sova block commits to.',
    note: 'Built; live on the public testnet.',
  },
  {
    n: 5, id: 'sip-5', file: 'sip-5-withdrawn.md', title: 'Wrapped ZEC', st: 'withdrawn',
    sum: 'Wrapped ZEC is a custodial product operated by Sova Labs at wz.cash, not a rule every node runs.',
    note: 'Withdrawn: moved out of the protocol.',
  },
  {
    n: 6, id: 'sip-6', file: 'sip-6-draft-sealer-signatures.md', title: 'Sealer signatures', st: 'accepted',
    sum: 'The sealer signs its block with the key its burn credits, so every block names its sealer and light clients get a signature to verify.',
    note: 'Accepted and built; live on the public testnet.',
  },
  {
    n: 7, id: 'sip-7', file: 'sip-7-draft-zcash-events.md', title: 'Zcash pool state and events', st: 'accepted',
    sum: 'Contracts read the value in each shielded pool and every change to it; each Sova block records a summary of its Zcash block in state.',
    note: 'Accepted and built; live on the public testnet.',
  },
  {
    n: 8, id: 'sip-8', file: 'sip-8-draft-anchored-burns.md', title: 'Anchored burns', st: 'accepted',
    sum: 'Each burn also names the Sova block it builds on, so Zcash records which history the miners chose, and rewriting it means out-burning them.',
    note: 'Accepted. Required for mainnet; on testnet after SIP-6.',
  },
] as const;

// Ashwings rendered by the contract (contracts/samples/), all twelve samples.
export const owls = [4, 1, 2, 0, 3, 5, 6, 7, 8, 9, 10, 11].map((n) => `/ashwings/onchain-${n}.svg`);

// The homepage directions (/v/block, /v/fire, /v/cover): three ways in and
// the reading links, shared so every direction carries the same doors.
// The commands are the tools' own (box/README.md, the miner's README,
// contracts/src/zcash/IZcash.sol).
export const doors = [
  { href: '/mine', h: 'Mine', cmd: 'sova-miner mine', t: 'Burn ZEC, earn SOVA.' },
  { href: '/node', h: 'Run a node', cmd: './box/up.sh', t: 'Zcash and Sova side by side, on a laptop.' },
  { href: '/build', h: 'Build', cmd: 'IZcash.txInfo()', t: 'Contracts that read Zcash.' },
] as const;
// Reading links, in the homepage's order (Rob, 2026-09-23): the whitepaper
// right before the SIPs, the SIPs last.
export const readLinks = [
  { href: '/ecosystem', label: 'Ecosystem' },
  { href: REPO, label: 'GitHub' },
  { href: repo.contributing, label: 'Contributing' },
  { href: '/paper', label: 'Whitepaper' },
  { href: '/sips', label: 'SIPs' },
] as const;

// The homepage's table of contents (/ and /v/cover), Rob's order of
// 2026-09-23: Why first, then Mine, then the other doors and links, then the
// Whitepaper, the SIPs last. `where` is the right-hand column: the path, the
// repo without its scheme, or the file name for a file in the repo.
export const coverToc = [
  { href: '/why', label: 'Why' },
  ...doors.map((d) => ({ href: d.href, label: d.h })),
  ...readLinks,
].map((t) => ({
  ...t,
  where: t.href.startsWith('https://') ? (t.href.split('/blob/main/')[1] ?? t.href.replace('https://', '')) : t.href,
}));

// Live testnet stats (components/TestnetStats.astro, filled in the browser by
// scripts/testnet-stats.ts from SOVA_RPC); `key` is the data-stat hook. Only
// what the chain states in one call each: the Sova height and its block's
// time (eth_getBlockByNumber) and the anchored Zcash height (ZcashBlocks
// latest(), SIP-7). No burns or SOVA-minted totals: no RPC exposes them, and
// summing every block's withdrawals is too many calls for a page view.
export const testnetStats = [
  { key: 'sova', label: 'sova height' },
  { key: 'zcash', label: 'zcash height' },
  { key: 'age', label: 'last block' },
] as const;

// Every indexable page besides the landing page at `/`, in the homepage's
// order (Rob, 2026-09-23): Why, Mine, the rest, then the Whitepaper and the
// SIPs, then the press kit. `nav` is the short label in the Site.astro header
// (nine items; /press is footer-only). The paper's title block and margin
// list, every footer and the sitemap use the full list.
export const sitePages = [
  { href: '/why', label: 'Why Sova', nav: 'why' },
  { href: '/mine', label: 'Mine', nav: 'mine' },
  { href: '/node', label: 'Run a node', nav: 'node' },
  { href: '/build', label: 'Build', nav: 'build' },
  { href: '/ecosystem', label: 'Ecosystem', nav: 'ecosystem' },
  { href: '/ashwings', label: 'Ashwings', nav: 'ashwings' },
  { href: '/story', label: 'Story', nav: 'story' },
  { href: '/paper', label: 'Whitepaper', nav: 'whitepaper' },
  { href: '/sips', label: 'SIPs', nav: 'sips' },
  { href: '/press', label: 'Press kit', nav: '' },
] as const;

// Ashwings species (contracts/src/Ashwings.sol `SPECIES_W`, `SPECIES_RGB`):
// body colour and mint weight, out of 256 on-chain, shown here rounded to
// percent (51/31/31/26/26/20/20/20/15/11/5 of 256).
export const species = [
  { name: 'classic', body: '#B0841D', w: 20 },
  { name: 'bright', body: '#F4B728', w: 12 },
  { name: 'bronze', body: '#967019', w: 12 },
  { name: 'barn', body: '#D8B56A', w: 10 },
  { name: 'moss', body: '#7B8F3E', w: 10 },
  { name: 'ember', body: '#C25A33', w: 8 },
  { name: 'dusk', body: '#7E6BA8', w: 8 },
  { name: 'slate', body: '#7C8792', w: 8 },
  { name: 'snowy', body: '#E8E4D8', w: 6 },
  { name: 'midnight', body: '#3E4A6B', w: 4 },
  { name: 'spectral', body: '#FBF3DC', w: 2 },
] as const;

// The twelve contract-rendered samples (contracts/samples/onchain-N.svg,
// seeds in contracts/samples/seeds.txt), each labelled with the species the
// contract derives from its seed.
export const owlSamples = [
  [0, 'snowy'], [1, 'barn'], [2, 'ember'], [3, 'bronze'], [4, 'classic'], [5, 'spectral'],
  [6, 'dusk'], [7, 'midnight'], [8, 'moss'], [9, 'classic'], [10, 'slate'], [11, 'bright'],
] as const;

// The footer's outbound links (every Site.astro page, the paper and the
// /v/* directions). An entry with no `href` is a placeholder until launch:
// rendered as a non-link with data-placeholder={key} (site/README.md,
// "Still at launch").
export const footerLinks: readonly { key: string; label: string; href?: string }[] = [
  { key: 'source-repo', label: 'Source', href: repo.root },
  { key: 'sips', label: 'Specs', href: repo.sips },
  { key: 'docs', label: 'Docs', href: repo.docs },
  { key: 'testnet', label: 'Testnet', href: repo.blob(TESTNET.guide) },
  { key: 'telegram', label: 'Telegram', href: SOCIAL.telegram },
  { key: 'x', label: 'X', href: SOCIAL.x },
];

// Sova wordmark paths (brand/SOVA LOGO/COLOR/SOVA_LOGO_COLOR.svg), viewBox 0 0 387 70.
export const WORDMARK_PATHS = [
  'M0 67.9644V56.1336H62.4266C67.2093 56.1336 70.7334 52.6095 70.7334 47.8268C70.7334 43.17 67.2093 39.6459 62.4266 39.6459H20.2635C8.55848 39.6459 1.2586 33.4788 1.2586 25.8013C1.2586 17.7463 9.18778 13.7187 16.3618 11.8308C15.1032 13.9705 14.5998 16.3618 14.5998 18.3756C14.5998 23.5358 17.7463 26.9341 23.9134 26.9341H64.5662C76.6488 26.9341 85.3331 35.6184 85.3331 47.4492C85.3331 59.2801 76.6488 67.9644 64.5662 67.9644H0ZM16.3618 11.8308C21.0186 4.02752 27.5634 0 38.2615 0H80.2987V11.8308H16.3618Z',
  'M153.412 0C173.927 0 188.023 14.0963 188.023 32.8495V35.115C188.023 53.8681 173.927 67.9644 153.412 67.9644H130.505C110.116 67.9644 95.8936 53.8681 95.8936 35.115V32.8495C95.8936 14.0963 110.116 0 130.505 0H153.412ZM110.997 35.2408C110.997 47.3234 119.052 55.3784 131.26 55.3784H152.656C164.991 55.3784 172.92 47.3234 172.92 35.2408V32.7236C172.92 20.641 164.991 12.586 152.656 12.586H131.26C119.052 12.586 110.997 20.641 110.997 32.7236V35.2408Z',
  'M237.614 69.9782C223.392 69.9782 215.337 63.0559 210.932 49.9664L193.941 0H209.421L224.776 45.5613C227.797 54.3715 233.712 57.2663 243.404 57.2663H256.996C254.857 63.8111 251.584 69.9782 237.614 69.9782ZM256.996 57.2663L276.253 0H291.734L276.882 43.0441C273.107 54.1198 266.939 57.2663 256.996 57.2663Z',
  'M319.287 67.9644C306.827 67.9644 298.143 59.2801 298.143 47.3234C298.143 35.3667 306.827 26.6823 319.287 26.6823H344.459C356.416 26.6823 364.219 30.7099 371.897 38.2615H322.182C316.896 38.2615 313.12 41.7855 313.12 46.8199C313.12 51.8543 316.896 55.3784 322.182 55.3784H371.897V38.2615C381.085 44.932 387 54.1198 387 65.3214V67.9644H319.287ZM371.897 24.7944C371.897 17.998 367.24 13.3412 360.444 13.3412H302.674V0H363.212C377.686 0 387 9.18779 387 23.5358V38.2615H371.897V24.7944Z',
];

// Two-burner split (docs/WORKPLAN.md C3, ladder scenario on regtest): the paid
// amounts are logged; the 5:2 weights and the tip are derived with the SIP-2
// math (tip = R - sum of floor pro-rata shares of 9R/10, in gwei), which
// reproduces the logged amounts exactly. See site/README.md.
export const twoBurnerSplit = {
  rows: [
    { rank: '0', weight: '5', tip: '625.000000001', share: '4,017.857142857', paid: '4,642.857142858' },
    { rank: '1', weight: '2', tip: '—', share: '1,607.142857142', paid: '1,607.142857142' },
  ],
  total: '6,250.000000000',
} as const;
