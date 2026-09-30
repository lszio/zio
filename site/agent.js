// Zio Agent & LLM Page — real WASM evaluation, no server, no pre-recorded animation.

let engine = null;

// ── Zio sources (validated against the wasm engine; no string escapes exist
//    in the reader, so mock LLM data is injected as map literals) ──
const scenarios = {
  toolAgent: `;; ── Tool-Calling Agent Loop（模拟 LLM 路由，本地离线规则）──
(def env {:weather {:city "Hangzhou" :temp-c 24 :cond "sunny"}
          :calc    {:expr "40 + 2" :value 42}})

(defn route [msg]
  (cond
    ((str-contains? msg "天气") :weather)
    ((str-contains? msg "计算") :calc)
    (else :none)))

(defn agent [msg]
  (let* [tool (route msg)]
    (cond
      ((= tool :weather)
       (let* [r (get env :weather)]
         (str "Tool[weather] → " (get r :city) " " (get r :temp-c) "°C " (get r :cond))))
      ((= tool :calc)
       (let* [r (get env :calc)]
         (str "Tool[calc] → " (get r :expr) " = " (get r :value))))
      (else (str "LLM → " msg " | 无可用工具，直接回答")))))

(list
  (agent "查一下 Hangzhou 天气")
  (agent "帮我计算 40 + 2")
  (agent "写一首诗"))`,

  cspAgents: `;; ── 多 Agent CSP 通道协作 ──
(def bus (chan 8))

(defn worker [name payload]
  (do
    (send! bus {:from name :msg payload})
    (str name " 已投递: " payload)))

(def a (worker "planner" "拆解任务: 抓取数据"))
(def b (worker "executor" "执行: 抓取数据"))

(def inbox (list (recv! bus) (recv! bus)))
(defn summarize [acc m]
  (str acc (get m :from) " → " (get m :msg) "；"))
(str "协调者收到 ⇒ " (reduce summarize "" inbox))`,

  llmPipeline: `;; ── LLM 结构化输出管道（模拟补全 → 提取 → JSON 序列化）──
(def mock-llm {:summary {:tldr "Zio 是通用 Lisp" :tokens 12}
               :extract {:entity "Zio" :score 95}})

(defn run-pipeline [kind text]
  (let* [c (get mock-llm kind)]
    (cond
      ((= kind :summary)
       (json-stringify {:kind "summary" :input text :tldr (get c :tldr) :tokens (get c :tokens)}))
      (else
       (json-stringify {:kind "extract" :input text :entity (get c :entity) :score (get c :score)})))))

(list
  (run-pipeline :summary "Zio 是一门通用 Lisp 语言")
  (run-pipeline :extract "Zio 与 Lisp 的关系"))`
};

const SCENARIO_OUT = { toolAgent: 'out-tool', cspAgents: 'out-csp', llmPipeline: 'out-pipeline' };
const SCENARIO_SRC = { toolAgent: 'src-tool', cspAgents: 'src-csp', llmPipeline: 'src-pipeline' };

function initWasm() {
  return import('./wasm/zio_core.js')
    .then(async (m) => { await m.default(); engine = m; return true; })
    .catch((err) => { console.warn('WASM load failed:', err); return false; });
}

// ── Training lab: 4-armed bandit; policy + Q-learning fully in Zio ──
const ARM_KEYS = ['q0', 'q1', 'q2', 'q3'];
const trueMeans = { q0: 700, q1: 500, q2: 300, q3: 100 }; // milli reward rates

const train = {
  running: false,
  q: {}, c: {},           // persistent Zio-side state, reinjected per batch
  runs: [],               // per-episode reward arrays
  seed: 987654321,
  epsMilli: 200,
  epsPerBatch: 12,
};

function nestPut(entries, keyFn) {
  return Object.entries(entries).reduce((acc, [k, v]) => `(put ${acc} ${keyFn(k)} ${v})`, '{}');
}

function fullQ() {
  const q = { q0: 0, q1: 0, q2: 0, q3: 0, ...train.q };
  const c = { c0: 0, c1: 0, c2: 0, c3: 0, ...train.c };
  return { q, c };
}

function buildProg(q, c, epsMilli, episodes, seed) {
  return `(def arms {:q0 700 :q1 500 :q2 300 :q3 100})   ; 环境真实回报率(毫单位)
(def K 4)
(def EPS-MILLI ${epsMilli})                            ; 探索率 ε × 1000
(def STEPS 20)                                          ; 每回合交互步数
(def EPISODES ${episodes})                              ; 本批回合数
(def initq ${nestPut(q, (k) => `:${k}`)})   ; Q 状态(宿主回注,跨批持久)
(def initc ${nestPut(c, (k) => `"${k}"`)})   ; 各臂拉动计数

(defn lcg [s] (mod (+ (* s 1103515245) 12345) 2147483648))

;; ε-greedy 决策:贪婪臂 = Q 值 argmax
(defn argmax [q i besti bestv s]
  (if (= i K)
    besti
    (let* [v (get q (keyword (str "q" i)))
           nb (if (> v bestv) i besti)
           nv (if (> v bestv) v bestv)]
      (argmax q (+ i 1) nb nv (lcg s)))))

;; 单步:选臂 → 环境给奖励 → Q 增量更新
(defn step [q c s n rewards]
  (if (= n STEPS)
    {:q q :c c :r rewards}
    (let* [s1 (lcg s)
           s2 (lcg s1)
           arm (if (< (mod s1 1000) EPS-MILLI) (mod s2 K) (argmax q 0 0 -1 s2))
           qkey (keyword (str "q" arm))
           mean (get arms qkey)
           s3 (lcg s2)
           reward (if (< (mod s3 1000) mean) 1 0)
           n-i (get c (str "c" arm))
           oldq (get q qkey)
           newq (+ oldq (/ (- (* reward 1000) oldq) (+ n-i 1)))]
      (step (put q qkey newq) (put c (str "c" arm) (+ n-i 1)) (lcg s3) (+ n 1) (cons reward rewards)))))

;; 回合循环:Q/C 跨回合持续演化
(defn run-ep [q c ep s acc]
  (if (= ep EPISODES)
    {:q q :c c :runs acc}
    (let* [res (step q c (lcg (+ s ep)) 0 (list))]
      (run-ep (get res :q) (get res :c) (+ ep 1) (lcg s) (cons (get res :r) acc)))))

(json-stringify (run-ep initq initc 0 ${seed} (list)))`;
}

function trainBatch() {
  const { q, c } = fullQ();
  const prog = buildProg(q, c, train.epsMilli, train.epsPerBatch, train.seed);
  document.getElementById('train-source').textContent = prog;
  let raw;
  try {
    raw = engine.eval_zio(prog);
  } catch (e) {
    return { error: String(e) };
  }
  try {
    // eval_zio returns the value's repr; json-stringify reprs as a JSON string literal
    const out = JSON.parse(JSON.parse(raw));
    train.seed = (train.seed * 1103515245 + 12345) % 2147483647;
    return out;
  } catch (e) {
    return { error: String(e) };
  }
}

function drawChart() {
  const canvas = document.getElementById('reward-chart');
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth, h = canvas.clientHeight;
  if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
    canvas.width = w * dpr; canvas.height = h * dpr;
  }
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const pad = { l: 30, r: 10, t: 10, b: 18 };
  ctx.font = '10px JetBrains Mono, monospace';
  ctx.fillStyle = '#64748b';

  // gridlines
  [0, 0.25, 0.5, 0.75, 1].forEach((v) => {
    const y = pad.t + (1 - v) * (h - pad.t - pad.b);
    ctx.strokeStyle = 'rgba(255,255,255,0.06)';
    ctx.beginPath(); ctx.moveTo(pad.l, y); ctx.lineTo(w - pad.r, y); ctx.stroke();
    ctx.fillText(v.toFixed(2), 2, y + 3);
  });

  const epAvg = train.runs.map((r) => r.reduce((a, b) => a + b, 0) / r.length);
  if (!epAvg.length) {
    ctx.fillText('点击「▶ 开始训练」', pad.l + 8, h / 2);
    return;
  }
  const x = (i) => pad.l + (epAvg.length === 1 ? 0 : (i / (epAvg.length - 1)) * (w - pad.l - pad.r));
  const y = (v) => pad.t + (1 - v) * (h - pad.t - pad.b);

  // per-episode reward dots
  ctx.fillStyle = 'rgba(127,0,255,0.55)';
  epAvg.forEach((v, i) => { ctx.beginPath(); ctx.arc(x(i), y(v), 1.6, 0, Math.PI * 2); ctx.fill(); });

  // moving average line (window 10)
  ctx.strokeStyle = '#00f2fe'; ctx.lineWidth = 2; ctx.beginPath();
  let started = false;
  for (let i = 0; i < epAvg.length; i++) {
    const lo = Math.max(0, i - 9);
    const ma = epAvg.slice(lo, i + 1).reduce((a, b) => a + b, 0) / (i - lo + 1);
    if (!started) { ctx.moveTo(x(i), y(ma)); started = true; } else { ctx.lineTo(x(i), y(ma)); }
  }
  ctx.stroke();

  // 0.5 reference (random policy baseline)
  ctx.strokeStyle = 'rgba(255,184,0,0.5)'; ctx.lineWidth = 1; ctx.setLineDash([4, 4]);
  ctx.beginPath(); ctx.moveTo(pad.l, y(0.5)); ctx.lineTo(w - pad.r, y(0.5)); ctx.stroke();
  ctx.setLineDash([]);
  ctx.fillStyle = '#ffb800';
  ctx.fillText('随机策略基线 0.5', w - pad.r - 96, y(0.5) - 4);
}

function renderQBars() {
  const { q, c } = fullQ();
  const fmtPulls = (n) => (n >= 1000 ? (n / 1000).toFixed(1) + 'k' : String(n));
  const totalPulls = ARM_KEYS.reduce((a, k) => a + (c['c' + k.slice(1)] || 0), 0);
  document.getElementById('q-bars').innerHTML = ARM_KEYS.map((k) => {
    const v = (q[k] || 0) / 1000;
    const pulls = c['c' + k.slice(1)] || 0;
    const pct = Math.max(0, Math.min(100, v * 100));
    return `<div class="q-bar-row">
      <span>臂 ${k.slice(1)} (μ=${(trueMeans[k] / 1000).toFixed(2)})</span>
      <div class="q-bar-track"><div class="q-bar-fill" style="width:${pct}%"></div></div>
      <span class="q-bar-val">Q=${v.toFixed(2)} ·${fmtPulls(pulls)}次</span>
    </div>`;
  }).join('') + `<div class="sim-note">Q 估计值 vs 真实回报率 μ — 训练充分后 Q 应收敛于 μ，且最优臂 0 拉动次数最多（当前共 ${fmtPulls(totalPulls)} 次）</div>`;
}

function renderStats() {
  const recent = train.runs.slice(-20).flat();
  const avg = recent.length ? (recent.reduce((a, b) => a + b, 0) / recent.length).toFixed(3) : '—';
  const { q } = fullQ();
  const best = ARM_KEYS.reduce((a, b) => ((q[b] || 0) > (q[a] || 0) ? b : a), 'q0');
  document.getElementById('train-stats').innerHTML =
    `<span class="stat-chip">回合 ${train.runs.length}</span>` +
    `<span class="stat-chip">平均奖励(最近20回合) ${avg}</span>` +
    `<span class="stat-chip">当前最优估计 臂${best.slice(1)}</span>` +
    `<span class="stat-chip">ε ${(train.epsMilli / 1000).toFixed(2)}</span>`;
}

function trainTick() {
  if (!train.running) return;
  const out = trainBatch();
  if (out.error) {
    train.running = false;
    setToggle();
    document.getElementById('train-stats').innerHTML =
      `<span class="stat-chip" style="color:#ff6b6b;border-color:#ff6b6b">求值错误: ${out.error}</span>`;
    return;
  }
  out.runs.slice().reverse().forEach((r) => train.runs.push(r));
  train.q = out.q; train.c = out.c;
  drawChart(); renderQBars(); renderStats();
  requestAnimationFrame(trainTick);
}

function setToggle() {
  document.getElementById('train-toggle').textContent = train.running ? '⏸ 暂停' : '▶ 开始训练';
}

function resetTrain() {
  train.running = false;
  train.q = {}; train.c = {}; train.runs = []; train.seed = 987654321;
  setToggle(); drawChart(); renderQBars(); renderStats();
}

function initTraining() {
  const toggle = document.getElementById('train-toggle');
  toggle.addEventListener('click', () => { train.running = !train.running; setToggle(); if (train.running) trainTick(); });
  document.getElementById('train-reset').addEventListener('click', resetTrain);
  const eps = document.getElementById('eps-slider');
  eps.addEventListener('input', () => {
    train.epsMilli = parseInt(eps.value, 10);
    document.getElementById('eps-val').textContent = (train.epsMilli / 1000).toFixed(2);
  });
  const speed = document.getElementById('speed-slider');
  speed.addEventListener('input', () => {
    train.epsPerBatch = parseInt(speed.value, 10) * 3;
    document.getElementById('speed-val').textContent = speed.value;
  });
  window.addEventListener('resize', drawChart);
  drawChart(); renderQBars(); renderStats();
}

// ── Playground ──
function unquote(s) {
  return s.startsWith('"') && s.endsWith('"') ? s.slice(1, -1) : s;
}

function runScenario(key) {
  const outEl = document.getElementById(SCENARIO_OUT[key]);
  outEl.classList.remove('err');
  outEl.textContent = '求值中…';
  setTimeout(() => {
    try {
      const raw = engine.eval_zio(scenarios[key]);
      outEl.textContent = unquote(raw);
    } catch (e) {
      outEl.classList.add('err');
      outEl.textContent = 'Error: ' + e;
    }
  }, 10);
}

function initScenarios() {
  for (const [key, src] of Object.entries(scenarios)) {
    document.getElementById(SCENARIO_SRC[key]).textContent = src;
  }
  document.querySelectorAll('.run-scenario').forEach((btn) => {
    btn.addEventListener('click', () => {
      if (!engine) return;
      runScenario(btn.getAttribute('data-scenario'));
    });
  });
}

// ── Boot ──
document.addEventListener('DOMContentLoaded', async () => {
  initScenarios();
  initTraining();
  const badge = document.getElementById('engine-badge');
  const ok = await initWasm();
  if (ok) {
    badge.textContent = '⚡ Real WASM Engine Active';
    badge.style.background = 'rgba(0,230,118,0.15)';
    badge.style.color = '#00e676';
    badge.style.borderColor = 'rgba(0,230,118,0.3)';
  } else {
    badge.textContent = 'WASM 加载失败';
    badge.style.color = '#ff6b6b';
    document.getElementById('train-source').textContent = ';; WASM 引擎加载失败，训练与试用不可用。';
    document.getElementById('train-toggle').disabled = true;
  }
});
