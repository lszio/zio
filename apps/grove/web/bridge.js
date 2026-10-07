// Grove browser bridge.
//
// This is the only JavaScript on the page. It is a thin generic layer
// between the persistent Zio/WASM session (which owns every product
// decision) and the DOM, network, storage, timer, and canvas.
//
// The bridge never:
//   - decides a route, role, or status interpretation,
//   - parses a response body,
//   - reads or stores the bearer token,
//   - decides what to render for predictions, runs, lineage, modules,
//     evaluations, approvals, signals, or comparisons,
//   - approves, refreshes, or publishes anything.
//
// Every product flow routes through the controller. The bridge executes
// exactly the commands the controller emits.

import wasmInit, { ZioSession } from '/wasm/zio_core.js';
import {
  CONTROLLER_SOURCE, CONTROLLER_PATH,
  BUNDLE_SOURCE, BUNDLE_PATH,
} from './controller-source.js';

const STORAGE_KEY = 'grove.filed';
const CONTROLLER_STEP_SYMBOL = 'grove--browser-step';
const BUNDLE_EXPORT_SYMBOL = 'grove--browser-export';

const state = {
  zio: null,
  controllerState: null,
  nonce: 0,
  pending: new Map(),
};

// ─── DOM primitives ──────────────────────────────────────────────────────

const $ = (sel) => {
  if (!sel || typeof sel !== 'string') return null;
  const el = sel.startsWith('#') ? document.getElementById(sel.slice(1))
    : sel.startsWith('.') ? document.querySelector(sel)
    : document.querySelector(sel);
  return el || null;
};

const setText = (id, t, kind) => {
  const el = $(id);
  if (!el) return;
  el.textContent = t == null ? '' : String(t);
  el.classList.remove('ok', 'error', 'warn');
  if (kind === 'error') el.classList.add('error');
  else if (kind === 'warn') el.classList.add('warn');
  else if (kind) el.classList.add(kind);
};

const setValue = (id, v) => {
  const el = $(id);
  if (!el) return;
  if ('value' in el) el.value = v == null ? '' : String(v);
};

const focusTarget = (id) => {
  const el = $(id);
  if (el && typeof el.focus === 'function') el.focus();
};

const setAttribute = (id, name, value) => {
  const el = $(id);
  if (!el) return;
  if (value === null || value === undefined || value === false) el.removeAttribute(name);
  else el.setAttribute(name, String(value));
};

const buildNode = (node) => {
  if (!node || typeof node !== 'object') return document.createTextNode('');
  const el = document.createElement(node.tag || 'div');
  if (node.attrs) {
    for (const [k, v] of Object.entries(node.attrs)) {
      if (v === null || v === undefined || v === false) continue;
      el.setAttribute(k, String(v));
    }
  }
  if (node.text != null && node.text !== '') {
    el.appendChild(document.createTextNode(String(node.text)));
  }
  if (Array.isArray(node.children)) {
    for (const child of node.children) {
      el.appendChild(buildNode(child));
    }
  }
  if (el.dataset.event) {
    el.addEventListener('click', (ev) => {
      ev.preventDefault();
      const form = el.closest('form');
      deliver({ kind: 'click', id: el.dataset.event,
        value: el.dataset.value || '', fields: readFields(form) });
    });
  }
  return el;
};

const replaceTarget = (target, children) => {
  const el = $(target);
  if (!el) return;
  el.replaceChildren();
  for (const child of children || []) {
    el.appendChild(buildNode(child));
  }
};

// ─── Canvas drawing primitives ──────────────────────────────────────────

const drawPixels = (target, pixels, width, height) => {
  const canvas = $(target);
  if (!canvas) return;
  if (canvas.width !== width) canvas.width = width;
  if (canvas.height !== height) canvas.height = height;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  const cw = canvas.width, ch = canvas.height;
  const cellW = cw / width;
  const cellH = ch / height;
  for (let r = 0; r < height; r++) {
    for (let c = 0; c < width; c++) {
      const idx = r * width + c;
      const v = (pixels && pixels[idx] != null) ? Number(pixels[idx]) | 0 : 0;
      const gray = Math.max(0, Math.min(255, v));
      ctx.fillStyle = `rgb(${gray},${gray},${gray})`;
      ctx.fillRect(Math.floor(c * cellW), Math.floor(r * cellH),
        Math.ceil(cellW), Math.ceil(cellH));
    }
  }
};

const drawSvg = (target, width, height, markup) => {
  const el = $(target);
  if (!el) return;
  // The controller emits a complete <svg ...>...</svg> document; render
  // it via the browser's HTML parser so namespace handling is consistent.
  if (el.tagName === 'CANVAS') {
    if (el.width !== width) el.width = width;
    if (el.height !== height) el.height = height;
    return;
  }
  // Setting innerHTML on a <div> with SVG markup keeps the SVG namespace
  // because the parser auto-detects it.
  el.innerHTML = markup || '';
};

// ─── Field collection ────────────────────────────────────────────────────

const STANDALONE_FIELD_IDS = new Set(['event-run']);

const readFields = (form) => {
  const out = {};
  if (form) {
    for (const el of form.elements) {
      if (!el.name && !el.id) continue;
      const key = el.id || el.name;
      if (!key) continue;
      if (el.type === 'checkbox') out[key] = el.checked;
      else out[key] = el.value;
    }
  }
  // The bridge also forwards a small set of page-level fields that the
  // controller expects on every event (the trace run id and the bearer
  // token). Reading them from the DOM is mechanical — the controller
  // decides what they mean.
  for (const id of STANDALONE_FIELD_IDS) {
    const el = document.getElementById(id);
    if (el && out[id] === undefined) out[id] = el.value;
  }
  // The bearer token is the user's input; the bridge copies its current
  // value into the event so the controller can attach the right header.
  // The token never leaves the bridge.
  const tokenEl = document.getElementById('token');
  if (tokenEl) out.token = tokenEl.value;
  return out;
};

// ─── Network ─────────────────────────────────────────────────────────────

const fetchJson = (cmd) => new Promise((resolve) => {
  const headers = Object.assign({}, cmd.headers || {});
  if (cmd.body !== null && cmd.body !== undefined) {
    headers['Content-Type'] = headers['Content-Type'] || 'application/json';
  }
  const path = (cmd.path || []).join('/');
  const qs = cmd.query && Object.keys(cmd.query).length
    ? '?' + new URLSearchParams(
      Object.entries(cmd.query).filter(([, v]) => v != null)
    ).toString() : '';
  const url = (path.startsWith('/') ? path : '/' + path) + qs;
  fetch(url, {
    method: cmd.method || 'GET',
    headers,
    body: cmd.body !== null && cmd.body !== undefined
      ? JSON.stringify(cmd.body) : undefined,
    credentials: 'same-origin',
  }).then(async (response) => {
    const text = await response.text();
    resolve({ status: response.status, body: text, error: null });
  }).catch((err) => {
    resolve({ status: 0, body: '', error: String(err && err.message || err) });
  });
});

// ─── Command dispatch ───────────────────────────────────────────────────

const timers = new Map();
const applyCommand = (cmd) => {
  if (!cmd || typeof cmd !== 'object') return Promise.resolve();
  switch (cmd.op) {
    case 'text':
      setText(cmd.target, cmd.text, cmd.class === 'error' ? 'error' : 'ok');
      return Promise.resolve();
    case 'value':
      setValue(cmd.target, cmd.value);
      return Promise.resolve();
    case 'focus':
      focusTarget(cmd.target);
      return Promise.resolve();
    case 'attribute':
      setAttribute(cmd.target, cmd.name, cmd.value);
      return Promise.resolve();
    case 'replace':
      replaceTarget(cmd.target, cmd.children);
      return Promise.resolve();
    case 'image':
    case 'canvas':
      drawPixels(cmd.target, cmd.pixels || [], cmd.width || 16, cmd.height || 16);
      return Promise.resolve();
    case 'svg':
      drawSvg(cmd.target, cmd.width, cmd.height, cmd.markup);
      return Promise.resolve();
    case 'storage-set':
      try { localStorage.setItem(cmd.key, String(cmd.value == null ? '' : cmd.value)); }
      catch (_) {}
      return Promise.resolve();
    case 'storage-get':
      return new Promise((resolve) => {
        let v = '[]';
        try { v = localStorage.getItem(cmd.key); } catch (_) {}
        deliver({ kind: 'storage', value: v || '[]',
          context: cmd.context || {} }).then(resolve);
      });
    case 'timer': {
      if (timers.has(cmd.key)) clearInterval(timers.get(cmd.key));
      const handle = setInterval(() => deliver({ kind: 'timer' }), cmd.milliseconds || 1000);
      timers.set(cmd.key, handle);
      return Promise.resolve();
    }
    case 'timer-cancel': {
      const h = timers.get(cmd.key);
      if (h) { clearInterval(h); timers.delete(cmd.key); }
      return Promise.resolve();
    }
    case 'confirm':
      return new Promise((resolve) => {
        let accepted = false;
        try { accepted = Boolean(window.confirm(cmd.message || 'Confirm?')); }
        catch (_) {}
        deliver({ kind: 'confirm', accepted, context: cmd.context || {} }).then(resolve);
      });
    case 'http':
      return fetchJson(cmd).then((result) => deliver({
        kind: 'response',
        id: cmd.context && cmd.context.purpose,
        context: cmd.context || {},
        status: result.status,
        body: result.body,
        error: result.error,
        fields: {},
      }));
    default:
      console.warn('[grove bridge] unknown op', cmd.op, cmd);
      return Promise.resolve();
  }
};

// ─── Controller step ────────────────────────────────────────────────────

const wrapJson = (value) => JSON.stringify(value == null ? null : value);

const callController = (event) => {
  if (!state.zio) return [];
  const evtJson = wrapJson({
    kind: event.kind || null,
    id: event.id || null,
    nonce: event.nonce != null ? event.nonce : state.nonce++,
    value: event.value != null ? event.value : null,
    fields: event.fields || {},
    context: event.context || {},
    status: event.status != null ? event.status : null,
    body: event.body != null ? event.body : null,
    error: event.error != null ? event.error : null,
  });
  const stateJson = wrapJson(state.controllerState);
  // Escape the JSON for embedding in a string literal in the wrapped
  // program, then have Zio parse it back into a map. The controller reads
  // via string keys (with a keyword fallback), so string-keyed maps are
  // correct.
  const escapedState = JSON.stringify(stateJson);
  const escapedEvent = JSON.stringify(evtJson);
  const wrapped =
    `(let [s (${CONTROLLER_STEP_SYMBOL} ` +
    `(json-parse ${escapedState}) (json-parse ${escapedEvent}))] ` +
    `{:state (get s :state) :commands (get s :commands)})`;
  let raw;
  try { raw = state.zio.eval_json(wrapped, '<bridge>'); }
  catch (e) { return [[ 'error', String(e && e.message || e) ]]; }
  let parsed = null;
  try { parsed = JSON.parse(raw); } catch (_) {
    return [['error', 'controller returned non-JSON: ' + raw]];
  }
  if (parsed && typeof parsed === 'object' && !Array.isArray(parsed) && parsed.kind === 'zio-error') {
    return [['error', parsed.message || 'controller error']];
  }
  if (!parsed || !Array.isArray(parsed.commands)) {
    return [['error', 'controller result missing commands']];
  }
  state.controllerState = parsed.state;
  return parsed.commands;
};

const deliver = async (event) => {
  const result = callController(event);
  if (Array.isArray(result) && result[0] && result[0][0] === 'error') {
    const el = document.getElementById('runtime-error');
    if (el) { el.hidden = false; el.textContent = result[0][1]; }
    return;
  }
  for (const cmd of result) await applyCommand(cmd);
};

// ─── Form / button / select wiring ──────────────────────────────────────

const wireForm = (form) => {
  if (!form || form.dataset.wired === '1') return;
  form.dataset.wired = '1';
  form.addEventListener('submit', (ev) => {
    ev.preventDefault();
    deliver({ kind: 'submit', id: form.id, fields: readFields(form) });
  });
};

const wireSelects = () => {
  for (const sel of document.querySelectorAll('select[id]')) {
    if (sel.dataset.wired === '1') continue;
    sel.dataset.wired = '1';
    sel.addEventListener('change', () => {
      deliver({ kind: 'change', id: sel.id,
        value: sel.value, fields: readFields(sel.form) });
    });
  }
};

const wireButtons = () => {
  for (const btn of document.querySelectorAll('button[id]')) {
    if (btn.dataset.wired === '1') continue;
    if (btn.type === 'submit') continue;
    btn.dataset.wired = '1';
    btn.addEventListener('click', (ev) => {
      ev.preventDefault();
      deliver({ kind: 'click', id: btn.id,
        value: btn.dataset.value || null,
        fields: readFields(btn.form) });
    });
  }
};

const wireToken = () => {
  const t = document.getElementById('token');
  if (!t || t.dataset.wired === '1') return;
  t.dataset.wired = '1';
  t.addEventListener('input', () => {
    deliver({ kind: 'click', id: 'token', value: t.value, fields: {} });
  });
};

// ─── Boot ────────────────────────────────────────────────────────────────

const announceError = (where, err) => {
  const el = document.getElementById('runtime-error');
  if (!el) return;
  el.hidden = false;
  el.textContent = `[${where}] ${err && err.message || String(err)}`;
  const status = document.getElementById('runtime-status');
  if (status) {
    status.textContent = 'Controller failed to start';
    status.classList.add('error');
  }
};

const loadController = async () => {
  // The trusted controller and bundle sources are bundled in
  // controller-source.js. The WASM host's BufferIoHost serves any
  // /<...>/<file>.zio path that add_source registered. The controller
  // source is the entire application business; stdlib is supplied by
  // the Foundation build at ZioSession::new.
  state.zio.add_source(CONTROLLER_PATH, CONTROLLER_SOURCE);
  state.zio.add_source(BUNDLE_PATH, BUNDLE_SOURCE);
  // `load` reads via IoHost. Pass an absolute path so the browser's
  // "no cwd" capability doesn't short-circuit file resolution.
  const loadResult = state.zio.eval(
    `(load "${BUNDLE_PATH}")`,
    '<browser-bundle>'
  );
  if (typeof loadResult === 'string' && loadResult.startsWith('Error:')) {
    throw new Error(loadResult);
  }
  // Evaluate the bundle's boot export. It returns
  // {:initialState ... :initialCommands ...}.
  const bundleJson = state.zio.eval_json(
    `(${BUNDLE_EXPORT_SYMBOL})`,
    '<bundle-export>'
  );
  const bundle = JSON.parse(bundleJson);
  state.controllerState = bundle.initialState || null;
  // Apply the initial commands immediately so the welcome UI is in place
  // before any user interaction.
  for (const cmd of bundle.initialCommands || []) {
    await applyCommand(cmd);
  }
};

const boot = async () => {
  try {
    await wasmInit('/wasm/zio_core_bg.wasm');
    state.zio = new ZioSession();
    await loadController();
    for (const form of document.querySelectorAll('form')) wireForm(form);
    wireSelects();
    wireButtons();
    wireToken();
    setAttribute('#surface', 'aria-busy', 'false');
    setText('#runtime-status',
      'Zio/WASM controller ready · AST bootstrap · explicit human approval', 'ok');
  } catch (err) {
    console.error(err);
    announceError('boot', err);
  }
};

if (typeof window !== 'undefined') {
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
}

export { deliver, applyCommand };
