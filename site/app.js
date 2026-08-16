// Zio Landing Page Interactive Application Script with WASM Integration

let wasmEngine = null;

document.addEventListener('DOMContentLoaded', async () => {
  initCodeTabs();
  initStatsCounter();
  await initWasmModule();
  initReplSimulator();
});

// Load WASM Engine
async function initWasmModule() {
  const badgeEl = document.getElementById('wasm-status-badge');
  try {
    const zioWasm = await import('./wasm/zio_core.js');
    await zioWasm.default();
    wasmEngine = zioWasm;
    console.log("Zio WASM Module initialized successfully!");
    if (badgeEl) {
      badgeEl.innerHTML = `⚡ Real WASM Engine Active (Rust + JS Interop)`;
      badgeEl.style.background = 'rgba(0, 230, 118, 0.15)';
      badgeEl.style.color = '#00e676';
      badgeEl.style.borderColor = 'rgba(0, 230, 118, 0.3)';
    }
  } catch (err) {
    console.warn("WASM module load fallback:", err);
    if (badgeEl) {
      badgeEl.innerHTML = `Simulator Mode`;
    }
  }
}

// Interactive REPL Simulator & WASM Terminal
function initReplSimulator() {
  const outputEl = document.getElementById('repl-output');
  const inputEl = document.getElementById('repl-input');
  const presetBtns = document.querySelectorAll('.preset-btn');

  const presets = {
    jseval: {
      title: "JS Interop (eval & DOM)",
      expr: '(js/eval "document.querySelector(\'.hero h1\').style.color = \'#00f2fe\'")',
      evals: [
        { expr: '(js/dom-set-text ".terminal-title" "⚡ Zio WASM Engine Active")', type: 'boolean' },
        { expr: '(js/console-log "Hello from real Zio WASM engine!")', type: 'nil' }
      ]
    },
    syntax: {
      title: "syntax-rules 卫生宏",
      expr: '(defmacro swap! (syntax-rules () (((swap! a b) (let [tmp a] (set! a b) (set! b tmp))))))',
      evals: [
        { expr: '(swap! x y)', type: 'sexp' }
      ]
    },
    zos: {
      title: "ZOS 类与多分派",
      expr: '(defclass point () ((x :initarg :x) (y :initarg :y)))',
      evals: [
        { expr: '(def p (make-instance point :x 10 :y 20))', type: 'object' },
        { expr: '(slot-value p :x)', type: 'integer' }
      ]
    },
    csp: {
      title: "CSP Channel 与并发",
      expr: '(def c (chan 5))',
      evals: [
        { expr: '(send! c "hello agent")', type: 'nil' },
        { expr: '(recv! c)', type: 'string' }
      ]
    },
    json: {
      title: "JSON & 结构化数据",
      expr: '(json-stringify {:agent "ZioBot" :status :active})',
      evals: [
        { expr: '(json-parse "{\"val\": 42}")', type: 'map' }
      ]
    }
  };

  function evaluateExpr(expr) {
    if (wasmEngine && typeof wasmEngine.eval_zio === 'function') {
      try {
        return wasmEngine.eval_zio(expr);
      } catch (e) {
        return `Error: ${e}`;
      }
    }
    return evaluateFallback(expr);
  }

  function runPreset(key) {
    const data = presets[key];
    if (!data) return;

    outputEl.innerHTML = '';

    // Line 1: Definition
    const res1 = evaluateExpr(data.expr);
    appendLine(data.expr, res1);

    // Subsequent evaluations
    if (data.evals) {
      data.evals.forEach(ev => {
        setTimeout(() => {
          const res = evaluateExpr(ev.expr);
          appendLine(ev.expr, res);
        }, 150);
      });
    }
  }

  function appendLine(expr, result) {
    const line = document.createElement('div');
    line.className = 'terminal-line-group';
    line.innerHTML = `
      <div class="terminal-line"><span class="prompt">zio&gt;</span> <span>${escapeHtml(expr)}</span></div>
      <div class="output">${escapeHtml(result)}</div>
    `;
    outputEl.appendChild(line);
    outputEl.scrollTop = outputEl.scrollHeight;
  }

  presetBtns.forEach(btn => {
    btn.addEventListener('click', () => {
      presetBtns.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
      const key = btn.getAttribute('data-preset');
      runPreset(key);
    });
  });

  if (inputEl) {
    inputEl.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        const val = inputEl.value.trim();
        if (val) {
          const res = evaluateExpr(val);
          appendLine(val, res);
          inputEl.value = '';
        }
      }
    });
  }

  // Load default preset
  runPreset('jseval');
}

function evaluateFallback(expr) {
  if (expr.startsWith('(js/eval')) return '"#<js-eval ok>"';
  if (expr.startsWith('(js/dom-set-text')) return 'true';
  if (expr.startsWith('(js/console-log')) return 'nil';
  if (expr.startsWith('(+') || expr.startsWith('(*')) return '15';
  if (expr.startsWith('(defclass')) return '#<Class point>';
  if (expr.startsWith('(def p')) return '#<point>';
  if (expr.startsWith('(slot-value')) return '10';
  if (expr.startsWith('(json-stringify')) return '"{\\"agent\\":\\"ZioBot\\",\\"status\\":\\"active\\"}"';
  return 'evaluated-ok';
}

function escapeHtml(str) {
  return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// Code Tabs Switcher
function initCodeTabs() {
  const tabBtns = document.querySelectorAll('.code-tab-btn');
  const codeContent = document.getElementById('code-display');

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
;; → (let [tmp__hyg_1 x] (do (set! x y) (set! y tmp__hyg_1)))`,

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
@p ;; → 42 (deref blocking)`
  };

  tabBtns.forEach(btn => {
    btn.addEventListener('click', () => {
      tabBtns.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
      const key = btn.getAttribute('data-snippet');
      if (snippets[key]) {
        codeContent.textContent = snippets[key];
      }
    });
  });
}

// Stats Counter Animation
function initStatsCounter() {
  const statNumbers = document.querySelectorAll('.stat-number');
  statNumbers.forEach(numEl => {
    const target = parseInt(numEl.getAttribute('data-target') || '0', 10);
    if (!target) return;
    let current = 0;
    const step = Math.max(1, Math.floor(target / 30));
    const timer = setInterval(() => {
      current += step;
      if (current >= target) {
        current = target;
        clearInterval(timer);
      }
      numEl.textContent = current;
    }, 30);
  });
}
