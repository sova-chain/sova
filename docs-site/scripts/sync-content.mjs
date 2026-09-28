#!/usr/bin/env node
// Builds src/content/docs/ (and a few static assets) from the repository, so
// every page has exactly one source:
//
//   - Repo markdown (the join guide, its reference, the box README, the
//     miner README, the SIPs) is copied with frontmatter added, its first
//     H1 used as the title, and repo-relative links rewritten to docs URLs
//     (or to GitHub for files the docs site doesn't render).
//   - Pages written for the docs site live in docs-site/pages/ and get the
//     same link rewriting, plus these directives:
//       <!-- include: path/in/repo.sol -->   the file, as a code block
//       <!-- generate: contracts -->         the testnet deploy record table
//       <!-- generate: rpc-allowlist -->     the public RPC method list
//       <!-- generate: address multicall3 --> one testnet contract address
//
// src/content/docs/, public/fonts/, public/favicon.svg and src/assets/ are
// generated: never edit them, they are gitignored and rebuilt on every
// `npm run dev` / `npm run build`.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SITE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const REPO = path.resolve(SITE, '..');
const OUT = path.join(SITE, 'src/content/docs');
const GITHUB = 'https://github.com/sova-chain/sova';

// Repo markdown rendered on the docs site: repo path -> docs slug, with the
// title (overrides the file's H1 when set) and description.
const REPO_PAGES = [
  {
    src: 'docs/guides/testnet.md',
    slug: 'start/quickstart',
    title: 'Quickstart: join the testnet',
    description: 'From nothing to a synced Zcash testnet node, a Sova node on the network, and a miner burning TAZ for SOVA.',
  },
  {
    src: 'box/up/README.md',
    slug: 'start/local',
    title: 'Run it locally',
    description: 'One command starts a Zcash regtest node, a Sova node and a miner on your machine.',
  },
  {
    src: 'docs/guides/testnet-reference.md',
    slug: 'reference/testnet',
    title: 'Testnet reference',
    description: 'Requirements, the snapshot, how SOVA is paid, sealing, earnings, troubleshooting and every setting of the public testnet.',
  },
  {
    src: 'crates/burn-wallet/miner/README.md',
    slug: 'reference/sova-miner',
    title: 'sova-miner CLI',
    description: 'Budget-capped burn mining: init, mine, report and export-evm-key, funding, budgets, broadcast and chain resets.',
  },
];

// Other repo paths that resolve to a docs page (aliases).
const ALIASES = { 'box/README.md': 'start/local' };

// SIP summaries, from the site's own list (site/src/data/sova.ts), so the
// SIP descriptions match sova.io/sips word for word.
function sipSummaries() {
  const ts = read('site/src/data/sova.ts');
  const out = {};
  const re = /n: (\d+), id: '[^']+', file: '([^']+)', title: '([^']+)', st: '([^']+)',\s*sum: '([^']*)',\s*note: '([^']*)'/g;
  for (const m of ts.matchAll(re)) {
    out[m[2]] = { n: Number(m[1]), title: m[3], status: m[4], sum: m[5], note: m[6] };
  }
  return out;
}

function read(rel) {
  return fs.readFileSync(path.join(REPO, rel), 'utf8');
}

function write(slug, body, ext = '.md') {
  const file = path.join(OUT, slug + ext);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, body);
}

function yamlString(s) {
  return JSON.stringify(s); // JSON strings are valid YAML scalars
}

// ---------------------------------------------------------------- routes

const routes = new Map(); // repo path -> docs URL
for (const p of REPO_PAGES) routes.set(p.src, `/${p.slug}/`);
for (const [src, slug] of Object.entries(ALIASES)) routes.set(src, `/${slug}/`);

const sips = fs
  .readdirSync(path.join(REPO, 'sips'))
  .filter((f) => /^sip-\d+.*\.md$/.test(f))
  .sort((a, b) => parseInt(a.slice(4)) - parseInt(b.slice(4)));
for (const f of sips) routes.set(`sips/${f}`, `/specs/sip-${parseInt(f.slice(4))}/`);
routes.set('sips', '/specs/');

// Authored pages: docs-site/pages/<slug>.md(x) -> /<slug>/
const PAGES_DIR = path.join(SITE, 'pages');
function walk(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? walk(path.join(dir, e.name)) : [path.join(dir, e.name)],
  );
}
const authored = walk(PAGES_DIR).filter((f) => /\.mdx?$/.test(f));
for (const f of authored) {
  const rel = path.relative(PAGES_DIR, f).replace(/\.mdx?$/, '');
  const slug = rel === 'index' ? '' : rel.replace(/\/index$/, '');
  routes.set(path.relative(REPO, f), slug ? `/${slug}/` : '/');
}

// ---------------------------------------------------------------- links

// Rewrite markdown links whose target is a repo-relative path. `from` is the
// source file's repo path. Code spans and fenced blocks are left alone.
function rewriteLinks(md, from) {
  const parts = md.split(/(```[\s\S]*?```|`[^`\n]*`)/g);
  return parts
    .map((part, i) => (i % 2 === 1 ? part : part.replace(/\]\(([^)\s]+)\)/g, (all, target) => `](${resolveLink(target, from)})`)))
    .join('');
}

function resolveLink(target, from) {
  if (/^[a-z]+:/i.test(target) || target.startsWith('#') || target.startsWith('/')) return target;
  const [p, hash = ''] = target.split('#');
  const abs = path.posix.normalize(path.posix.join(path.posix.dirname(from), p)).replace(/\/$/, '');
  const anchor = hash ? `#${hash}` : '';
  if (routes.has(abs)) return routes.get(abs) + anchor;
  const full = path.join(REPO, abs);
  if (!fs.existsSync(full)) throw new Error(`${from}: link to missing repo path ${target} (${abs})`);
  const kind = fs.statSync(full).isDirectory() ? 'tree' : 'blob';
  return `${GITHUB}/${kind}/main/${abs}${anchor}`;
}

// ---------------------------------------------------------------- directives

function codeLang(file) {
  const ext = path.extname(file).slice(1);
  return { sol: 'solidity', rs: 'rust', ts: 'ts', js: 'js', json: 'json', toml: 'toml', sh: 'bash' }[ext] ?? '';
}

function include(rel) {
  const text = read(rel).replace(/\s+$/, '');
  return '```' + codeLang(rel) + ` title="${rel}"\n${text}\n` + '```';
}

function contractsTable() {
  const d = JSON.parse(read('infra/testnet/deployments/sova-testnet.json'));
  const names = {
    wsova: 'WSOVA (wrapped SOVA)',
    factory: 'Uniswap-v2-class factory',
    router: 'Uniswap-v2-class router',
    multicall3: 'Multicall3',
    ashwings: 'Ashwings',
    market: 'Ashwings market',
  };
  const rows = Object.entries(d.deployments).map(([k, v]) => {
    const name = names[k] ?? k;
    return `| ${name} | \`${v.address}\` | \`${v.artifact}\` | ${v.block} |`;
  });
  return [
    `Chain ID \`${d.chainId}\`, genesis \`${d.genesisHash}\`.`,
    '',
    '| Contract | Address | Artifact | Block |',
    '| --- | --- | --- | --- |',
    ...rows,
  ].join('\n');
}

function rpcAllowlist() {
  const rs = read('bin/sova/src/rpc.rs');
  const block = rs.match(/PUBLIC_RPC_METHODS: &\[&str\] = &\[([\s\S]*?)\n\];/);
  if (!block) throw new Error('bin/sova/src/rpc.rs: PUBLIC_RPC_METHODS not found');
  // Each comment block labels the methods under it, up to the next comment.
  const groups = [];
  let lastWasComment = false;
  for (const raw of block[1].split('\n')) {
    const line = raw.trim();
    if (line.startsWith('//')) {
      const text = line.replace(/^\/\/\s?/, '');
      if (lastWasComment) groups.at(-1).label += ` ${text}`;
      else groups.push({ label: text, methods: [] });
      lastWasComment = true;
      continue;
    }
    const m = line.match(/^"([a-zA-Z0-9_]+)",?$/);
    if (!m) continue;
    if (!groups.length) groups.push({ label: '', methods: [] });
    groups.at(-1).methods.push(m[1]);
    lastWasComment = false;
  }
  // Short labels: a comment's first clause ("Broadcast and wait for the
  // receipt (EIP-7966), up to ..." -> "Broadcast and wait for the receipt").
  const short = (label) => label.split(/ \(|[;,.]/)[0].trim();
  return ['| Group | Methods |', '| --- | --- |']
    .concat(groups.map((g) => `| ${short(g.label)} | ${g.methods.map((x) => `\`${x}\``).join(', ')} |`))
    .join('\n');
}

function directives(md) {
  return md
    .replace(/<!-- include: ([^\s]+) -->/g, (_, rel) => include(rel))
    .replace(/<!-- generate: contracts -->/g, () => contractsTable())
    .replace(/<!-- generate: rpc-allowlist -->/g, () => rpcAllowlist())
    .replace(/<!-- generate: sova-usage -->/g, () => {
      const m = read('bin/sova/src/main.rs').match(/const USAGE: &str = "([\s\S]*?)";/);
      if (!m) throw new Error('bin/sova/src/main.rs: USAGE not found');
      return '```text\n' + m[1] + '\n```';
    })
    .replace(/<!-- generate: sip-table -->/g, () => {
      const sum = sipSummaries();
      const rows = sips.map((f) => {
        const n = parseInt(f.slice(4));
        const s = sum[f];
        if (!s) throw new Error(`site/src/data/sova.ts has no entry for sips/${f}`);
        const st = s.status[0].toUpperCase() + s.status.slice(1);
        return `| [SIP-${n}](/specs/sip-${n}/) | ${s.title} | ${st} | ${s.sum} ${s.note} |`;
      });
      return ['| SIP | Title | Status | What it does |', '| --- | --- | --- | --- |', ...rows].join('\n');
    })
    .replace(/<!-- generate: address (\w+) -->/g, (_, key) => {
      const d = JSON.parse(read('infra/testnet/deployments/sova-testnet.json'));
      if (!d.deployments[key]) throw new Error(`no deployment named ${key}`);
      return '`' + d.deployments[key].address + '`';
    });
}

// ---------------------------------------------------------------- pages

function frontmatter(fields) {
  const lines = Object.entries(fields)
    .filter(([, v]) => v !== undefined)
    .map(([k, v]) =>
      typeof v === 'object'
        ? `${k}:\n${Object.entries(v).map(([sk, sv]) => `  ${sk}: ${yamlString(sv)}`).join('\n')}`
        : `${k}: ${typeof v === 'string' ? yamlString(v) : v}`,
    );
  return `---\n${lines.join('\n')}\n---\n\n`;
}

// Split off the first H1: [title, rest].
function takeTitle(md) {
  const m = md.match(/^# (.+)\n+/m);
  if (!m || md.slice(0, m.index).trim()) return [undefined, md];
  return [m[1].trim(), md.slice(m.index + m[0].length)];
}

const editUrl = (src) => `${GITHUB}/edit/main/${src}`;

fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });

for (const p of REPO_PAGES) {
  const [h1, body] = takeTitle(read(p.src));
  const fm = frontmatter({ title: p.title ?? h1, description: p.description, editUrl: editUrl(p.src) });
  write(p.slug, fm + rewriteLinks(body, p.src));
}

const summaries = sipSummaries();
for (const f of sips) {
  const src = `sips/${f}`;
  const n = parseInt(f.slice(4));
  const [h1, body] = takeTitle(read(src));
  const s = summaries[f];
  const fm = frontmatter({
    title: h1 ?? `SIP-${n}`,
    description: s?.sum,
    sidebar: { label: `SIP-${n}: ${s?.title ?? (h1 ?? '').replace(/^SIP-\d+:\s*/, '')}`, order: n },
    editUrl: editUrl(src),
  });
  write(`specs/sip-${n}`, fm + rewriteLinks(body, src));
}

for (const f of authored) {
  const rel = path.relative(PAGES_DIR, f);
  const src = path.relative(REPO, f);
  const ext = path.extname(f);
  let md = fs.readFileSync(f, 'utf8');
  // Authored pages carry their own frontmatter; add the edit link.
  md = md.replace(/^---\n/, `---\neditUrl: ${yamlString(editUrl(src))}\n`);
  write(rel.slice(0, -ext.length), rewriteLinks(directives(md), src), ext);
}

// ---------------------------------------------------------------- assets

function copy(fromRel, toRel) {
  const to = path.join(SITE, toRel);
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(path.join(REPO, fromRel), to);
}
copy('site/public/favicon.svg', 'public/favicon.svg');
copy('site/public/fonts/roboto-mono-latin-wght.woff2', 'public/fonts/roboto-mono-latin-wght.woff2');
copy('site/public/fonts/LICENSE-RobotoMono.txt', 'public/fonts/LICENSE-RobotoMono.txt');
copy('brand/logo/sova-wordmark-gold.svg', 'src/assets/sova-wordmark-gold.svg');
copy('brand/logo/sova-wordmark-deep-gold.svg', 'src/assets/sova-wordmark-deep-gold.svg');

console.log(`sync: ${REPO_PAGES.length} repo pages, ${sips.length} SIPs, ${authored.length} authored pages -> src/content/docs`);
