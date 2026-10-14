// Documentation rich-text layer, loaded sitewide from Base.astro.
// Four upgrades, all client-side so they apply uniformly to Astro's markdown
// pipeline (docs/book/blog) and the reference pages built from `;; doc:`
// comments:
//   1. Lisp-family code blocks get a real highlighter — Shiki's lisp grammar
//      paints everything one color, so zio/lisp/clojure blocks are re-tokenized
//      here with nesting-depth rainbow parens.
//   2. Paren pairs: hover previews the match, click highlights both ends,
//      clicking an already-highlighted paren jumps (scrolls) to its partner.
//   3. ```mermaid fences render as diagrams (dynamic import keeps the
//      mermaid bundle off pages that don't use it).
//   4. Prose tables get a horizontal-scroll wrapper for narrow viewports.

const LISP_LANGS = new Set(['zio', 'lisp', 'clojure']);

// Special forms and core macros a reader should recognize at a glance.
const KEYWORDS = new Set([
  'def', 'defn', 'defmacro', 'defstruct', 'defprotocol', 'defentity',
  'defclass', 'defgeneric', 'defmethod', 'defpackage', 'defmulti',
  'fn', 'let', 'let*', 'loop', 'recur', 'if', 'cond', 'do', 'try',
  'quote', 'quasiquote', 'unquote', 'unquote-splicing', 'set!',
  'module', 'require', 'export', 'and', 'or', 'not', 'else',
  'nil', 'true', 'false',
]);

const PAIRS = { '(': ')', '[': ']', '{': '}' };
const CLOSE = new Set([')', ']', '}']);
const DEPTHS = 8; // rainbow palette size, class rp-0 … rp-7

function escapeHtml(text) {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// Tokenize one line of Lisp-family source into HTML. Line-based like the
// editors it imitates: strings and comments cannot span lines in fenced docs.
function highlightLine(line, state) {
  let out = '';
  let i = 0;
  const emit = (cls, text) => { out += `<span class="${cls}">${escapeHtml(text)}</span>`; };
  while (i < line.length) {
    const ch = line[i];
    if (state.inComment) { emit('tk-c', line.slice(i)); break; }
    if (ch === ';' && line[i + 1] === ';') { state.inComment = true; continue; }
    if (ch === '"' ) {
      let j = i + 1;
      while (j < line.length && !(line[j] === '"' && line[j - 1] !== '\\')) j++;
      emit('tk-s', line.slice(i, Math.min(j + 1, line.length)));
      i = j + 1;
      continue;
    }
    if (PAIRS[ch] !== undefined || CLOSE.has(ch)) {
      emit(`rp-${state.depth % DEPTHS}`, ch);
      state.depth += PAIRS[ch] !== undefined ? 1 : -1;
      if (state.depth < 0) state.depth = 0;
      i++;
      continue;
    }
    if (/\s/.test(ch)) { out += escapeHtml(ch); i++; continue; }
    // One atom: symbol, number, :keyword, #(...) dispatch, ~@ punct group.
    let j = i;
    while (j < line.length && !/[\s()"[\]{}]/.test(line[j])) j++;
    const atom = line.slice(i, j);
    if (atom.startsWith(':')) emit('tk-kw', atom);
    else if (KEYWORDS.has(atom)) emit('tk-k', atom);
    else if (/^[-+]?\d+(\.\d+)?$/.test(atom)) emit('tk-n', atom);
    else if (/^[#'"`~,]/.test(atom)) { // reader macros stick to the next atom
      const lead = atom.match(/^[#'"`~,]+/)[0];
      emit('tk-m', lead);
      const rest = atom.slice(lead.length);
      if (rest) {
        if (KEYWORDS.has(rest)) emit('tk-k', rest);
        else if (/^[-+]?\d+(\.\d+)?$/.test(rest)) emit('tk-n', rest);
        else emit('tk-p', rest);
      }
    } else emit('tk-p', atom);
    i = j;
  }
  return out;
}

// Re-render one shiki block from its plain text, then link matched paren
// spans with a data-pid id so interaction never re-parses.
function enhanceLispBlock(pre) {
  const code = pre.querySelector('code');
  if (!code) return;
  const source = code.textContent.replace(/\n$/, '');
  const state = { depth: 0, inComment: false };
  const html = source.split('\n').map((line) => {
    state.inComment = false;
    return `<span class="line">${highlightLine(line, state) || ''}</span>`;
  }).join('\n');
  code.innerHTML = html;
  // Pair ids: walk the emitted spans, matching with a stack. data-pid links
  // open and close so interaction never re-parses.
  const stack = [];
  code.querySelectorAll('.rp-0,.rp-1,.rp-2,.rp-3,.rp-4,.rp-5,.rp-6,.rp-7').forEach((span) => {
    const ch = span.textContent;
    if (PAIRS[ch] !== undefined) {
      stack.push(span);
    } else {
      const open = stack.pop();
      if (open) {
        const id = `p${pairCounter++}`;
        open.dataset.pid = id;
        span.dataset.pid = id;
      }
    }
  });
  pre.classList.add('zio-rich');
}

// ── paren pair interaction ────────────────────────────────────────────

// Pair ids are allocated across ALL blocks (a page has many code blocks;
// per-block counters would collide and one click would pin dozens of pairs).
let pairCounter = 0;
let selectedPid = null;

function parensByPid(pid) {
  return document.querySelectorAll(`[data-pid="${pid}"]`);
}

function setSelected(pid) {
  document.querySelectorAll('.hp-sel').forEach((el) => el.classList.remove('hp-sel'));
  selectedPid = pid;
  if (pid === null) return;
  parensByPid(pid).forEach((el) => el.classList.add('hp-sel'));
}

function wireParens() {
  document.addEventListener('click', (event) => {
    const span = event.target.closest('.rp-0,.rp-1,.rp-2,.rp-3,.rp-4,.rp-5,.rp-6,.rp-7');
    if (!span || !span.dataset.pid) return;
    const pid = span.dataset.pid;
    if (selectedPid === pid) {
      // Second click on the same pair: jump to the *other* end (SLIME-style
      // matching navigation), then leave both highlighted.
      const other = Array.from(parensByPid(pid)).find((el) => el !== span);
      if (other) other.scrollIntoView({ block: 'center', behavior: 'smooth' });
      other?.classList.add('hp-flash');
      setTimeout(() => other?.classList.remove('hp-flash'), 1200);
    } else {
      setSelected(pid);
    }
  });
  document.addEventListener('mouseover', (event) => {
    const span = event.target.closest('.rp-0,.rp-1,.rp-2,.rp-3,.rp-4,.rp-5,.rp-6,.rp-7');
    if (!span || !span.dataset.pid || span.dataset.pid === selectedPid) return;
    parensByPid(span.dataset.pid).forEach((el) => el.classList.add('hp-hover'));
  });
  document.addEventListener('mouseout', (event) => {
    const span = event.target.closest('.rp-0,.rp-1,.rp-2,.rp-3,.rp-4,.rp-5,.rp-6,.rp-7');
    if (!span) return;
    parensByPid(span.dataset.pid ?? '').forEach((el) => el.classList.remove('hp-hover'));
  });
}

// ── mermaid ────────────────────────────────────────────────────────────

async function renderMermaid() {
  const blocks = document.querySelectorAll('pre.astro-code[data-language="mermaid"]');
  if (blocks.length === 0) return;
  const nodes = [];
  blocks.forEach((pre) => {
    const div = document.createElement('div');
    div.className = 'mermaid';
    div.textContent = pre.querySelector('code')?.textContent ?? pre.textContent;
    pre.replaceWith(div);
    nodes.push(div);
  });
  const mermaid = (await import('mermaid')).default;
  mermaid.initialize({
    startOnLoad: false,
    theme: 'dark',
    fontFamily: 'Inter, system-ui, sans-serif',
  });
  await mermaid.run({ nodes });
}

// ── tables ────────────────────────────────────────────────────────────

function wrapTables() {
  document.querySelectorAll('.prose table').forEach((table) => {
    if (table.parentElement?.classList.contains('table-wrap')) return;
    const wrap = document.createElement('div');
    wrap.className = 'table-wrap';
    table.before(wrap);
    wrap.appendChild(table);
  });
}

// ── entry ─────────────────────────────────────────────────────────────

async function enhance() {
  const lispBlocks = document.querySelectorAll(
    'pre.astro-code[data-language="zio"],pre.astro-code[data-language="lisp"],pre.astro-code[data-language="clojure"]',
  );
  lispBlocks.forEach(enhanceLispBlock);
  wireParens();
  wrapTables();
  await renderMermaid().catch(() => { /* a broken diagram must not blank the page */ });
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', enhance);
} else {
  enhance();
}
