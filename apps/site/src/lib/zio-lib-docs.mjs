import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

// Doc-comment convention (docs/libdoc-convention.md):
//   - Leading `;;` block at the top of a file = file doc (first line is the title).
//   - `;; ── Section ──…` banner lines start a section.
//   - A contiguous `;;`/`;; doc:` block directly above a top-level `(def…)` form
//     (no blank line between) is that symbol's doc. First line should be
//     "`name` — 摘要".
//   - Lines indented ≥2 spaces after a blank comment line render as a code block.
// Banner lines are section markers, never doc text.

const BANNER = /^;;\s*─+\s*(.+?)\s*─+\s*$/;
const DEFFORM = /^\((def|defn|defmacro|defstruct|defprotocol|defentity|defclass|defgeneric|defmethod)\s+([^\s\][()]+)/;

function stripComment(line) {
  return line
    .replace(/^;;\s*doc:\s?/, '')
    .replace(/^;;\s?/, '')
    .replace(/\s+$/, '');
}

function docToMarkdown(lines) {
  const out = [];
  let code = null;
  for (const raw of lines) {
    const text = stripComment(raw);
    const indent = text.length - text.trimStart().length;
    if (indent >= 2 && text.trim() !== '') {
      if (code === null) code = [];
      code.push(text.trimStart());
    } else {
      if (code !== null) {
        out.push('```clojure\n' + code.join('\n') + '\n```');
        code = null;
      }
      out.push(text.trim() === '' ? '' : text);
    }
  }
  if (code !== null) out.push('```clojure\n' + code.join('\n') + '\n```');
  return out.join('\n');
}

/** Parse one .zio file into { title, description, sections: [{ title, symbols }] }. */
export function parseZioDoc(text) {
  const lines = text.split('\n');
  // File doc: contiguous ;; block from the first line.
  let i = 0;
  const header = [];
  while (i < lines.length && lines[i].startsWith(';;')) header.push(lines[i++]);
  const headerTitle =
    stripComment(header[0] ?? '').replace(/^─+\s*|\s*─+$/g, '').trim() || '未命名';
  const headerRest = header.slice(1).filter((l) => !BANNER.test(l));

  const sections = [];
  let section = { title: '概览', symbols: [] };
  sections.push(section);
  let doc = [];
  let flushPending = false; // a blank line ended the doc block

  const takeDoc = () => {
    if (doc.length === 0) return null;
    const d = doc;
    doc = [];
    return d;
  };

  for (; i < lines.length; i++) {
    const line = lines[i];
    const banner = line.match(BANNER);
    if (banner) {
      takeDoc();
      section = { title: banner[1].trim(), symbols: [] };
      sections.push(section);
      flushPending = false;
      continue;
    }
    if (line.startsWith(';;')) {
      if (flushPending) doc = []; // separated by blank line → not this form's doc
      flushPending = false;
      doc.push(line);
      continue;
    }
    const form = line.match(DEFFORM);
    if (form !== null && line.startsWith('(')) {
      const d = takeDoc();
      const argsMatch = line.match(/\[(.*?)\]/);
      section.symbols.push({
        name: form[2],
        kind: form[1],
        args: argsMatch ? argsMatch[1].trim() : '',
        doc: d ? docToMarkdown(d) : '',
      });
      flushPending = false;
      continue;
    }
    if (line.trim() === '' || (!line.startsWith(' ') && line.trim() !== '')) {
      flushPending = doc.length > 0;
    }
  }

  return {
    title: headerTitle,
    description: docToMarkdown(headerRest).trim(),
    sections: sections.filter((s) => s.symbols.length > 0),
  };
}

function walkZio(dir, out = []) {
  for (const name of readdirSync(dir).sort()) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walkZio(p, out);
    else if (name.endsWith('.zio')) out.push(p);
  }
  return out;
}

/**
 * Content-layer loader: parse doc comments out of the repo's Zio libraries
 * (libs/) and expose one collection entry per file, rendered as markdown.
 */
export function zioLibDocs(libsDir) {
  return {
    name: 'zio-lib-docs',
    load: async ({ store, renderMarkdown, generateDigest }) => {
      for (const path of walkZio(libsDir)) {
        const rel = relative(libsDir, path);
        const parsed = parseZioDoc(readFileSync(path, 'utf8'));
        const id = rel.replace(/\.zio$/, '').replace(/\//g, '-');
        const symbols = parsed.sections.flatMap((s) =>
          s.symbols.map((x) => ({ name: x.name, kind: x.kind })),
        );
        const body = [
          parsed.description,
          ...parsed.sections.map(
            (s) =>
              `## ${s.title}\n\n` +
              s.symbols
                .map(
                  (sym) =>
                    `### \`${sym.name}\`\n\n` +
                    '```clojure\n' +
                    `(${sym.kind} ${sym.name}${sym.args ? ' [' + sym.args + ']' : ''} …)\n` +
                    '```\n\n' +
                    (sym.doc || '_无文档。_'),
                )
                .join('\n'),
          ),
        ].join('\n\n');
        const rendered = await renderMarkdown(body);
        store.set({
          id,
          data: {
            title: parsed.title,
            file: rel,
            symbolCount: symbols.length,
            symbols,
          },
          body,
          rendered,
          digest: generateDigest(body),
        });
      }
    },
  };
}
