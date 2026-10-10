// Shared real WASM REPL for the homepage and dedicated language playground.

import { loadEngine } from './lib/engine.js';

let engine = null;

const presets = {
  basics: {
    lines: ['(defn square [x] (* x x))', '(square 7)'],
  },
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

// langs/cli/tests/site_presets.rs covers the original native programs;
// js/* builtins are WASM-only (langs/core/src/wasm.rs).
const snippets = {
  jsinterop: `;; JS 互操作：JS 对象是一等值，用 -> 链式调用
;; js/eval 返回真值：字符串/数字直接映射，对象成为 #<js-object> 句柄

;; 点分路径直接调用 JS 全局方法（this 自动绑定到父对象）
(js/console.log "hello from dotted call")
(println "Math.max:" (js/Math.max 1 42 7))
(println "Math.floor:" (-> 3.7 (js/Math.floor)))

;; Zio 复合数据深转换为 JS 数据
(println "json:" (js/JSON.stringify {:a 1 :b [1 2]}))

;; 句柄存变量后属性写入 + 方法调用
(def hero (js/eval "document.querySelector('.hero h1')"))
(js/set hero "textContent" "⚡ Zio WASM Engine Active")
(-> hero (js/call "getAttribute" "class") js/console-log)`,

  syntax: `;; syntax-rules hygiene subset
(def x 1)
(def y 2)
(defmacro swap! (syntax-rules () (((swap! a b) (let [tmp a] (set! a b) (set! b tmp))))))
(swap! x y)
(list x y) ;; → (2 1)`,

  zos: `;; ZOS Minimal Object System (CLOS / AMOP Subtype)
(defclass shape ()
  ((name :initarg :name)))

(defclass circle (shape)
  ((radius :initarg :radius)))

;; Generic Function & Multi-Dispatch
(defgeneric area (s))

(defmethod area ((c circle))
  (* 3.14159 (slot-value c :radius) (slot-value c :radius)))

(def c (make-instance circle :radius 5))
(area c) ;; → 78.53975`,

  csp: `;; Synchronous queue and future placeholders — not concurrency
(def c (chan 10))
(send! c {:agent "Agent-01" :status :ready})
(recv! c)

;; future-call evaluates eagerly; no async scheduling or blocking guarantee
(def f (future-call (fn [] (+ 20 22))))
(deref f) ;; → 42`,
};


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
  try {
    return engine.eval(expr);
  } catch (err) {
    return `Error: ${err instanceof Error ? err.message : String(err)}`;
  }
}

function runPreset(key, outputEl) {
  const data = presets[key];
  if (!data) return;
  if (!engine) return;
  const inputEl = document.getElementById('repl-input');
  if (inputEl) inputEl.value = data.lines.join('\n');
  outputEl.replaceChildren();
  // A persistent ZioSession evaluates the whole editable program in order.
  const program = data.lines.join('\n');
  appendLine(outputEl, program, evaluate(program));
}

export function initRepl() {
  const outputEl = document.getElementById('repl-output');
  const inputEl = document.getElementById('repl-input');
  const badgeEl = document.getElementById('wasm-status-badge');
  const formEl = document.getElementById('repl-form');
  const runEl = document.getElementById('repl-run');
  if (!outputEl || !inputEl || !formEl || !runEl) return;
  const presetBtns = document.querySelectorAll('.preset-btn');

  presetBtns.forEach((btn) => {
    btn.addEventListener('click', () => {
      presetBtns.forEach((b) => b.classList.remove('active'));
      btn.classList.add('active');
      runPreset(btn.getAttribute('data-preset'), outputEl);
    });
  });

  formEl.addEventListener('submit', (e) => {
    e.preventDefault();
    const val = inputEl.value.trim();
    if (!val || !engine) return;
    appendLine(outputEl, val, evaluate(val));
  });
  inputEl.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      formEl.requestSubmit();
    }
  });

  loadEngine()
    .then((e) => {
      engine = e;
      if (badgeEl) {
        badgeEl.textContent = '⚡ Real WASM Engine Active (Rust + JS Interop)';
        badgeEl.classList.add('is-live');
      }
      runEl.disabled = false;
      runPreset('basics', outputEl);
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
