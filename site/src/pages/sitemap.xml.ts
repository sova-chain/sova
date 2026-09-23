// sitemap.xml for the indexable pages: the landing page at / and every page
// in sitePages (the paper at /paper included). The /v/* design explorations
// and the unlisted /ashwings/{buy,mint,market} and /pulse pages are noindex and stay out.
import type { APIRoute } from 'astro';
import { sitePages } from '../data/sova';

export const GET: APIRoute = ({ site }) => {
  const paths = ['/', ...sitePages.map((p) => p.href)];
  const urls = paths.map((p) => `  <url><loc>${new URL(p, site).href}</loc></url>`).join('\n');
  const xml = `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${urls}\n</urlset>\n`;
  return new Response(xml, { headers: { 'Content-Type': 'application/xml; charset=utf-8' } });
};
