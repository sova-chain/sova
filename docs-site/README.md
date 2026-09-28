# docs.sova.io

The Sova documentation site: [Starlight](https://starlight.astro.build)
(Astro's docs framework), static output, themed to match sova.io.

## One source for every page

Nothing under `src/content/docs/` is written by hand. `npm run sync`
(run by `npm run dev` and `npm run build`) generates it from the repo:

| Docs page | Source |
| --- | --- |
| Quickstart: join the testnet | `docs/guides/testnet.md` |
| Testnet reference | `docs/guides/testnet-reference.md` |
| Run it locally | `box/up/README.md` |
| sova-miner CLI | `crates/burn-wallet/miner/README.md` |
| Specs (SIP-1 to SIP-8) | `sips/*.md` (descriptions from `site/src/data/sova.ts`) |
| Everything else | `docs-site/pages/**` |

The sync script (`scripts/sync-content.mjs`) adds frontmatter, takes the
first `# ` heading as the title, and rewrites repo-relative links: to the
docs page when the target is rendered here, to GitHub otherwise. A link
to a missing file fails the sync. So GitHub readers and docs readers read
the same files.

Pages in `pages/` can also pull from the repo at build time:

- `<!-- include: contracts/src/zcash/IZcash.sol -->`: the file, as a code block.
- `<!-- generate: contracts -->`: the testnet deploy record
  (`infra/testnet/deployments/sova-testnet.json`) as a table.
- `<!-- generate: address multicall3 -->`: one address from it.
- `<!-- generate: rpc-allowlist -->`: the public RPC methods, from
  `bin/sova/src/rpc.rs` (`PUBLIC_RPC_METHODS`).
- `<!-- generate: sova-usage -->`: `sova`'s usage text, from `bin/sova/src/main.rs`.
- `<!-- generate: sip-table -->`: the SIP list.

To add a repo file as a page, add it to `REPO_PAGES` in the script and to
the sidebar in `astro.config.mjs`. Every page's "Edit page" link opens its
real source on GitHub.

## Build

Node 22.12 or later.

```bash
cd docs-site
npm ci
npm run dev       # http://localhost:4321
npm run build     # sync + static build to dist/, then the link check
npm run preview   # serve dist/
```

The build fails on any broken internal link or anchor
(`starlight-links-validator`), so a renamed heading in a repo file breaks
the build rather than the site. Search is Pagefind, built into `dist/`.

## Deploy (Vercel, team `sovalabs`, docs.sova.io)

Static files only: no server, no functions. `vercel.json` sets the build
(`npm ci`, `npm run build`, output `dist`, trailing-slash redirects).

The build reads files outside `docs-site/` (`sips/`, `docs/guides/`, the
READMEs, `contracts/src/zcash/`, `bin/sova/src/`, `site/`, `brand/`), so
Vercel needs the whole repository, not only this directory.

**Option A: Git.** New project `sova-docs` in team `sovalabs`, imported
from `github.com/sova-chain/sova` (the public export, once it contains
`docs-site/`):

1. Root Directory: `docs-site`. Framework preset: Astro (the other
   settings come from `vercel.json`).
2. Keep "Include files outside the root directory in the Build Step" on
   (the default).
3. Node.js version: 22.x.
4. Production branch: `main`.

**Option B: CLI, from a checkout** (no Git connection):

```bash
cd docs-site
npx vercel link --scope sovalabs --project sova-docs   # once; creates the project if needed
npx vercel build --prod
npx vercel deploy --prebuilt --prod
```

**Domain.** In the project's Domains, add `docs.sova.io`. Vercel shows
the record to create; for a subdomain it is a `CNAME` from `docs` to
`cname.vercel-dns.com` (DNS only, not proxied, if the zone is on
Cloudflare).

**After the first deploy:** open `https://docs.sova.io/`, search for
"faucet", and check `/start/quickstart/#3b-get-taz-from-the-faucet`
(sova.io's Ashwings buy page links to that anchor on GitHub; the docs
page keeps it).
