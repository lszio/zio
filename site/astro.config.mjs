// @ts-check
import { defineConfig } from 'astro/config';
import mdx from '@astrojs/mdx';

// The site is served under /site/ (nginx: `location = / { return 302 /site/ }`,
// vercel.json rewrites everything into /site/). `base` must match, or every
// emitted asset URL misses the prefix and 404s.
export default defineConfig({
  site: 'https://zio.lszio.space',
  base: '/site',
  integrations: [mdx()],
});
