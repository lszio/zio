// @ts-check
import { defineConfig } from 'astro/config';
import mdx from '@astrojs/mdx';
import { satteri } from '@astrojs/markdown-satteri';
import repositoryLinks from './src/lib/repository-links.mjs';
import { fileURLToPath } from 'node:url';
import { getBuildInfo } from './src/lib/build-info.mjs';

// Served from the domain root: / is the landing page, /grove and /agent are
// real routes. `base` stays '/' — setting it to '/site' (the old layout) emits
// every asset and link under that prefix, so /grove alone 404s.
export default defineConfig({
  site: 'https://zio.lszio.space',
  base: '/',
  integrations: [mdx()],
  markdown: { processor: satteri({ mdastPlugins: [repositoryLinks] }) },
  vite: {
    define: {
      'import.meta.env.ZIO_BUILD_INFO': JSON.stringify(getBuildInfo(fileURLToPath(new URL('../../', import.meta.url)))),
    },
  },
});
