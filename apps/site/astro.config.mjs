// @ts-check
import { defineConfig } from 'astro/config';
import mdx from '@astrojs/mdx';
import { satteri } from '@astrojs/markdown-satteri';
import repositoryLinks from './src/lib/repository-links.mjs';
import { fileURLToPath } from 'node:url';
import { getBuildInfo } from './src/lib/build-info.mjs';

// Served from the domain root: / is the landing page; /agent, /intro, /docs,
// /libraries, /reference and /playground are the real routes. `base` stays '/'
// — setting it to '/site' (the old layout) emits every asset and link under
// that prefix, so the routes alone 404.
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
