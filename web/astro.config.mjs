// @ts-check
import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

// https://astro.build/config
export default defineConfig({
  // The site is served at the apex of its own custom domain, so there is no
  // `base` — every internal link in `src/` is a hardcoded root-relative path
  // (`href="/docs/terminology"`), and at a domain root those resolve as
  // written. A GitHub Pages *project* URL would serve from `/shep/` instead
  // and 404 every one of them; the custom domain is what makes them correct.
  //
  // `site` is also the sitemap integration's one prerequisite: every URL it
  // emits is this value plus the route, so a wrong value here ships a sitemap
  // pointing at a domain nobody serves.
  site: 'https://shep-pm.com',

  // Writes dist/sitemap-index.xml and dist/sitemap-0.xml over every built
  // page. public/robots.txt names the index, which is how a crawler that
  // never followed a link to a docs page finds all thirty of them.
  integrations: [sitemap()],
});
