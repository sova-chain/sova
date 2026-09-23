// @ts-check
import { defineConfig } from 'astro/config';

// Static output only: `npm run build` writes plain files to dist/, which is
// what Cloudflare Pages serves. No adapter, no server, no client framework.
export default defineConfig({
  site: 'https://sova.io',
  output: 'static',
  trailingSlash: 'ignore',
  build: {
    // One small stylesheet: inline it so the page is a single HTML request
    // plus fonts and images.
    inlineStylesheets: 'always',
  },
  devToolbar: { enabled: false },
});
