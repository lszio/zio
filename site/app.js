// Zio Landing Page Interactive Application Script

document.addEventListener('DOMContentLoaded', () => {
  initReplSimulator();
  initCodeTabs();
  initStatsCounter();
});

// Interactive REPL Simulator
function initReplSimulator() {
  const outputEl = document.getElementById('repl-output');
  const inputEl = document.getElementById('repl-input');
  const presetBtns = document.querySelectorAll('.preset-btn');

  const presets = {
    basics: {
      expr: '(defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))',
      result: '#<function (n)>',
      type: 'function',
      evals: [
        { expr: '(fib 10)', result: '55', type: 'integer' }
      ]
    },
    macro: {
      expr: '(defmacro unless [test body] (list \'if test nil body))',
      result: '#<macro unless (test body)>',
      type: 'macro',
      evals: [
        { expr: '(unless false 42)', result: '42', type: 'integer' }
      ]
    },
    syntax: {
      expr: '(defmacro swap! (syntax-rules () (((swap! a b) (let [tmp a] (set! a b) (set! b tmp))))))',
      result: '#<macro swap! (a b)>',
      type: 'macro',
      evals: [
        { expr: '(swap! x y)', result: '(let [tmp__hyg_1 x] (do (set! x y) (set! y tmp__hyg_1)))', type: 'sexp' }
      ]
    },
    zos: {
      expr: '(defclass point () ((x :initarg :x) (y :initarg :y)))',
      result: '#<Class Point>',
      type: 'class',
      evals: [
        { expr: '(def p (make-instance point :x 10 :y 20))', result: '#<Point>', type: 'object' },
        { expr: '(slot-value p :x)', result: '10', type: 'integer' }
      ]
    },
    csp: {
      expr: '(def c (chan 5))',
      result: '#<channel>',
      type: 'channel',
      evals: [
        { expr: '(send! c "hello agent")', result: 'nil', type: 'nil' },
        { expr: '(recv! c)', result: '"hello agent"', type: 'string' }
      ]
    }
  };

  function runPreset(key) {
    const data = presets[key];
    if (!data) return;

    outputEl.innerHTML = '';

    // Line 1: Definition
    appendLine(data.expr, data.result, data.type);

    // Subsequent evaluations
    if (data.evals) {
      data.evals.forEach(ev => {
        setTimeout(() => {
          appendLine(ev.expr, ev.result, ev.type);
        }, 150);
      });
    }
  }

  function appendLine(expr, result, type) {
    const line = document.createElement('div');
    line.className = 'terminal-line-group';
    line.innerHTML = `
      <div class="terminal-line"><span class="prompt">zio&gt;</span> <span>${escapeHtml(expr)}</span></div>
      <div class="output">${escapeHtml(result)} <span class="output-type">&lt;${type}&gt;</span></div>
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
          appendLine(val, evaluateSimulated(val), 'evaluated');
          inputEl.value = '';
        }
      }
    });
  }

  // Load default preset
  runPreset('basics');
}

function evaluateSimulated(expr) {
  if (expr.startsWith('(+') || expr.startsWith('(*')) {
    return '15';
  } else if (expr.startsWith('(bytes')) {
    return '#<buffer len=5>';
  } else if (expr.startsWith('(file-exists?')) {
    return 'true';
  } else if (expr.startsWith('(class-of')) {
    return 'Point';
  } else {
    return 'evaluated-ok';
  }
}

function escapeHtml(str) {
  return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// Code Tabs Switcher
function initCodeTabs() {
  const tabBtns = document.querySelectorAll('.code-tab-btn');
  const codeContent = document.getElementById('code-display');

  const snippets = {
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
@p ;; → 42 (deref blocking)`,

    protocol: `;; zio-protocol Extension Library
(require :zio.protocol)

(defprotocol Drawable
  (draw [this]))

(extend-type circle Drawable
  (draw [this]
    (println "Drawing circle with radius" (slot-value this :radius))))`
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
