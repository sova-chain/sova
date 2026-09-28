// @ts-check
// docs.sova.io: Starlight, static output. Content is generated into
// src/content/docs/ by scripts/sync-content.mjs (run by `npm run build`
// and `npm run dev`) from the repository's own markdown, so nothing here
// is a second copy. See README.md.
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';

export default defineConfig({
  site: 'https://docs.sova.io',
  output: 'static',
  trailingSlash: 'always',
  devToolbar: { enabled: false },
  integrations: [
    starlight({
      title: 'Sova docs',
      description: 'Documentation for Sova, the EVM that reads Zcash.',
      logo: {
        dark: './src/assets/sova-wordmark-gold.svg',
        light: './src/assets/sova-wordmark-deep-gold.svg',
        alt: 'Sova',
        replacesTitle: true,
      },
      favicon: '/favicon.svg',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/sova-chain/sova' },
        { icon: 'telegram', label: 'Telegram', href: 'https://t.me/sovazec' },
      ],
      components: {
        SocialIcons: './src/components/HeaderLinks.astro',
      },
      customCss: ['./src/styles/sova.css'],
      editLink: { baseUrl: 'https://github.com/sova-chain/sova/edit/main/' },
      lastUpdated: false,
      pagination: true,
      head: [
        { tag: 'meta', attrs: { name: 'theme-color', content: '#0c0a07' } },
        {
          tag: 'link',
          attrs: { rel: 'preload', href: '/fonts/roboto-mono-latin-wght.woff2', as: 'font', type: 'font/woff2', crossorigin: '' },
        },
      ],
      expressiveCode: {
        themes: ['github-dark-default', 'github-light'],
        styleOverrides: {
          borderRadius: '0',
          borderColor: 'var(--sova-line)',
          codeFontFamily: "'Roboto Mono', ui-monospace, Menlo, Consolas, monospace",
          uiFontFamily: "'Roboto Mono', ui-monospace, Menlo, Consolas, monospace",
          frames: {
            editorBackground: 'var(--sova-code-bg)',
            terminalBackground: 'var(--sova-code-bg)',
            editorTabBarBackground: 'var(--sova-code-bar)',
            terminalTitlebarBackground: 'var(--sova-code-bar)',
            editorActiveTabBackground: 'var(--sova-code-bg)',
            editorActiveTabIndicatorTopColor: 'var(--sl-color-accent)',
            frameBoxShadowCssValue: 'none',
          },
        },
      },
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'What is Sova', slug: 'start/what-is-sova' },
            { label: 'Quickstart: join the testnet', slug: 'start/quickstart' },
            { label: 'Run it locally', slug: 'start/local' },
            { label: 'Mine', slug: 'start/mine' },
            { label: 'Mint an Ashwing', slug: 'start/ashwings' },
          ],
        },
        {
          label: 'Build',
          items: [
            { label: 'Connect a wallet', slug: 'build/network' },
            { label: 'Contracts', slug: 'build/contracts' },
            { label: 'Read Zcash from a contract', slug: 'build/reading-zcash' },
            { label: 'Timing', slug: 'build/timing' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Testnet reference', slug: 'reference/testnet' },
            { label: 'sova-miner CLI', slug: 'reference/sova-miner' },
            { label: 'Node configuration', slug: 'reference/node' },
            { label: 'RPC methods', slug: 'reference/rpc' },
          ],
        },
        {
          label: 'Specs',
          items: [{ autogenerate: { directory: 'specs' } }],
        },
        {
          label: 'More',
          items: [
            { label: 'Whitepaper', link: 'https://sova.io/paper', attrs: { target: '_blank' } },
            { label: 'Explorer', link: 'https://explorer.testnet.sova.io', attrs: { target: '_blank' } },
            { label: 'Faucet', link: 'https://faucet.testnet.sova.io', attrs: { target: '_blank' } },
          ],
        },
      ],
      plugins: [starlightLinksValidator({ errorOnRelativeLinks: true, errorOnLocalLinks: true })],
    }),
  ],
});
