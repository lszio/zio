import { dirname, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const repositoryRoot = fileURLToPath(new URL('../../../../', import.meta.url));

// Keep root Markdown canonical: relative document links become site routes,
// while source-file links point to the actual repository, never a fake page.
export default {
  name: 'repository-links',
  link(node, ctx) {
    if (!ctx.fileURL || !node.url || /^(?:[a-z][a-z\d+.-]*:|\/|#)/i.test(node.url)) return;
    const [, target, suffix = ''] = node.url.match(/^([^?#]*)(.*)$/);
    const path = relative(repositoryRoot, resolve(dirname(fileURLToPath(ctx.fileURL)), decodeURI(target))).split(sep).join('/');
    if (path.startsWith('../')) return;
    const url = /^(docs|book|blog)\/.+\.md$/.test(path)
      ? `/${path.replace(/\.md$/, '')}/${suffix}`
      : `https://github.com/lszio/zio/blob/main/${path}${suffix}`;
    ctx.setProperty(node, 'url', url);
  },
};
