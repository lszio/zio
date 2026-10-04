// Landing page: real WASM REPL, code tabs, stats counter.
// Logic is unchanged from the previous site/app.js — only the engine load
// moved into ../lib/engine.ts so the BASE_URL prefix is resolved once.

import { loadEngine } from './lib/engine.js';

let engine = null;

const presets = {
  jseval: {
    lines: [
      '(js/eval "document.querySelector(\'.hero h1\').style.color = \'#00f2fe\'")',
      '(js/dom-set-text ".terminal-title" "⚡ Zio WASM Engine Active")',
      '(js/console-log "Hello from real Zio WASM engine!")',
    ],
  },
  syntax: {
    lines: [
      '(def x 1)',
      '(def y 2)',
      '(defmacro swap! (syntax-rules () (((swap! a b) (let [tmp a] (set! a b) (set! b tmp))))))',
      '(swap! x y)',
      '(list x y)',
    ],
  },
  zos: {
    lines: [
      '(defclass point () ((x :initarg :x) (y :initarg :y)))',
      '(def p (make-instance point :x 10 :y 20))',
      '(slot-value p :x)',
    ],
  },
  csp: {
    lines: [
      '(def c (chan 5))',
      '(send! c "hello agent")',
      '(recv! c)',
    ],
  },
  json: {
    lines: [
      '(json-stringify {:agent "ZioBot" :status :active})',
      '(json-parse "{\\"val\\": 42}")',
    ],
  },
};

// cli/tests/site_presets.rs pins these cumulative programs natively; the
// jseval preset is WASM-only (js/* builtins exist only in core/src/wasm.rs).
const snippets = {
  jsinterop: `;; Real-time JavaScript & Web Interoperability in WASM
;; Execute arbitrary JS expressions directly from Zio:
(js/eval "document.querySelector('.hero h1').style.color = '#00f2fe'")

;; Manipulate DOM elements natively:
(js/dom-set-text ".terminal-title" "⚡ Zio WASM Engine Active")

;; Output to browser console:
(js/console-log "Hello from real Zio WASM engine in browser!")`,

  syntax: `;; Hygienic syntax-rules Macro Engine (ADR-010 Phase 2)
(defmacro swap! [a b]
  (syntax-rules ()
    ((swap! a b)
     (let [tmp a]
       (set! a b)
       (set! b tmp)))))

;; Hygienic expansion avoids variable capture:
(swap! x y)
;; → (let [tmp__hyg_1 x] (set! x y) (set! y tmp__hyg_1))`,

  zos: `;; ZOS Minimal Object System (CLOS / AMOP Subtype)
(defclass shape ()
  ((name :initarg :name)))

(defclass circle (shape)
  ((radius :initarg :radius)))

;; Generic Function & Multi-Dispatch
(defgeneric area (s))

(defmethod area ((c circle))
  (* 3.14159 (slot-value c :radius) (slot-value c :radius)))

(def c (make-instance 'circle :radius 5))
(area c) ;; → 78.53975`,

  csp: `;; CSP Channels & Asynchronous Futures
(def c (chan 10))

;; Send & Recv
(send! c {:agent "Agent-01" :status :ready})
(recv! c) ;; → {:agent "Agent-01" :status :ready}

;; Promise & Deliver
(def p (promise))
(deliver p 42)
(deref p) ;; → 42 (blocking)`,
};

function escapeHtml(str) {
  return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function appendLine(outputEl, expr, result) {
  const line = document.createElement('div');
  line.className = 'terminal-line-group';
  const cmd = document.createElement('div');
  cmd.className = 'terminal-line';
  const prompt = document.createElement('span');
  prompt.className = 'prompt';
  prompt.textContent = 'zio>';
  const src = document.createElement('span');
  src.textContent = expr;
  cmd.append(prompt, src);
  const out = document.createElement('div');
  out.className = 'output';
  out.textContent = result; // textContent: engine output is untrusted
  line.append(cmd, out);
  outputEl.append(line);
  outputEl.scrollTop = outputEl.scrollHeight;
}

function evaluate(expr) {
  if (!engine) return 'WASM 引擎尚未就绪';
  return engine.eval(expr);
}

function runPreset(key, outputEl) {
  const data = presets[key];
  if (!data) return;
  outputEl.innerHTML = '';
  // engine is a persistent ZioSession: lines evaluate in order and
  // definitions from earlier lines stay visible.
  data.lines.forEach((expr, i) => {
    setTimeout(() => appendLine(outputEl, expr, evaluate(expr)), 150 * (i + 1));
  });
}

export function initRepl() {
  const outputEl = document.getElementById('repl-output');
  const inputEl = document.getElementById('repl-input');
  const badgeEl = document.getElementById('wasm-status-badge');
  const presetBtns = document.querySelectorAll('.preset-btn');

  presetBtns.forEach((btn) => {
    btn.addEventListener('click', () => {
      presetBtns.forEach((b) => b.classList.remove('active'));
      btn.classList.add('active');
      runPreset(btn.getAttribute('data-preset'), outputEl);
    });
  });

  inputEl?.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter') return;
    const val = inputEl.value.trim();
    if (!val) return;
    appendLine(outputEl, val, evaluate(val));
    inputEl.value = '';
  });

  loadEngine()
    .then((e) => {
      engine = e;
      if (badgeEl) {
        badgeEl.textContent = '⚡ Real WASM Engine Active (Rust + JS Interop)';
        badgeEl.classList.add('is-live');
      }
      runPreset('jseval', outputEl);
    })
    .catch((err) => {
      console.error('WASM load failed:', err);
      if (badgeEl) {
        badgeEl.textContent = 'WASM 引擎加载失败';
        badgeEl.classList.add('is-failed');
      }
    });
}

export function initCodeTabs() {
  const codeContent = document.getElementById('code-display');
  if (!codeContent) return;
  const tabBtns = document.querySelectorAll('.code-tab-btn');
  const show = (key) => {
    if (snippets[key]) codeContent.textContent = snippets[key];
  };
  tabBtns.forEach((btn) => {
    btn.addEventListener('click', () => {
      tabBtns.forEach((b) => b.classList.remove('active'));
      btn.classList.add('active');
      show(btn.getAttribute('data-snippet'));
    });
  });
  show('jsinterop');
}

export function initStatsCounter() {
  document.querySelectorAll('.stat-number').forEach((numEl) => {
    const target = parseInt(numEl.getAttribute('data-target') || '0', 10);
    if (!target) return;
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    let current = 0;
    const step = Math.max(1, Math.floor(target / 30));
    const timer = setInterval(() => {
      current += step;
      if (current >= target) {
        current = target;
        clearInterval(timer);
      }
      numEl.textContent = String(current);
    }, 30);
  });
}
