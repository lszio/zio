// @ts-check
import { defineConfig } from 'astro/config';
import mdx from '@astrojs/mdx';

// Served from the domain root: / is the landing page, /grove and /agent are
// real routes. `base` stays '/' — setting it to '/site' (the old layout) emits
// every asset and link under that prefix, so /grove alone 404s.
export default defineConfig({
  site: 'https://zio.lszio.space',
  base: '/',
  integrations: [mdx()],
});
