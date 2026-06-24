# Zio 路线图 — v0.2 → v1.0

> 从 103 tests 的 Lisp 内核到生产级通用语言

## 概要

**当前**: v0.2 — 架构重构完成。0 thread-local 全局变量。103 tests passing, 0 warnings。  
**目标**: v1.0 — 自托管的、可嵌入的系统编程 Lisp，附带 Datalog 数据库和 AutoML 框架。  
**时间线**: 预计 6-8 个月，7 个 Phase，每 Phase 2-4 周。

---

## 一览

```
v0.2 ─── 核心稳定化 ─── v0.3 ─── 系统编程 ─── v0.4 ─── 宏与元编程 ─── v0.5
  │                                                        │
  │  Span 嵌入 Sexp                                       │  卫生宏
  │  Reader 扩展                                          │  Reader macro
  │  通用 TCO                                             │  Compiler macro
  │  macroexpand 实现                                      │  Code walker
  │  更多错误类型                                          │
  │  测试 103 → 200+                                      │
  │                                                        │
  ▼                                                        ▼
v0.5 ─── 标准库+并发 ─── v0.6 ─── CLOS/多方法 ─── v0.7 ─── LLM/性能 ─── v1.0
  │                                                        │
  │  核心 stdlib (.zio)                                   │  向量原语
  │  懒序列                                                │  LLM eval
  │  Future/Promise/Channel                               │  Agent 框架
  │  Async/await macro                                    │  Baseline JIT
  │  包管理器                                              │  编译到机器码
  │  Test 框架                                            │
  │                                                        │
  └────────────────────────────────────────────────────────┘
                              │
                              ▼
                     ┌────────────────┐
                     │  v1.0 里程碑     │
                     │  - 自托管核心    │
                     │  - 嵌入友好      │
                     │  - Datalog 可用  │
                     │  - AutoML 原型   │
                     │  - LSP server    │
                     └────────────────┘
```

---

## Phase 1: 核心稳定化 (1-2 周)

**目标**: Zio 成为可靠的 Lisp 内核 — 错误消息可用、reader 完整、测试覆盖足够。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 1.1 | `Span` 嵌入 `Sexp` | 🔴 P0 | `sexp.rs`, `reader.rs`, `eval.rs`, `error.rs` | +80, -40 | 无 |
| 1.2 | Reader 扩展 (`#()` `#{}` `#\c` `,@`) | 🔴 P0 | `reader/src/reader.rs` | +100 | 1.1 |
| 1.3 | `macroexpand` 实现 | 🟡 P1 | `builtins.rs`, `macros.rs` | +30 | 无 |
| 1.4 | 通用 TCO (所有尾位置) | 🟡 P1 | `eval.rs` | +20, -5 | 无 |
| 1.5 | 错误类型扩展 | 🟡 P1 | `error.rs` | +50 | 无 |
| 1.6 | 测试覆盖提升 | 🟡 P1 | 全模块 | +200 | 1.1-1.3 |

### 交付标准

- `cargo test` → 200+ tests passing
- `cargo clippy` → 0 warnings
- `(try (car 1) (catch "not a list" msg))` 返回有用错误
- `(defn countdown [n] (if (zero? n) "done" (countdown (dec n))))` 不爆栈

---

## Phase 2: 系统编程基础 (2-4 周)

**目标**: 能写文件的系统程序 — FFI、buffer、file I/O、结构体。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 2.1 | FFI 原型 (C ABI) | 🔴 P0 | [new] `ffi.rs` | +200 | 无 |
| 2.2 | `#[zio_export]` proc-macro | 🔴 P0 | [new] `zio-macros/` crate | +150 | 2.1 |
| 2.3 | `Value::Buffer` 字节向量 | 🟡 P1 | `value.rs`, `builtins.rs` | +80 | 无 |
| 2.4 | File I/O builtins | 🟡 P1 | `builtins.rs` | +120 | 无 |
| 2.5 | `(defstruct ...)` 结构体 | 🟡 P1 | [new] `special/struct.rs` | +150 | 无 |
| 2.6 | `(try/catch)` + stack trace | 🟡 P1 | `eval.rs`, `error.rs` | +100 | 1.1 |

### 交付标准

```lisp
;; FFI 可用
(def sqrt (ffi/c "libm.so.6" "sqrt" f64 -> f64))
(println (sqrt 25.0))       ;; → 5.0

;; 文件操作
(def data (slurp "/etc/hostname"))
(spit "/tmp/out.txt" data)

;; 结构体
(defstruct Point [x y])
(def p (Point 10 20))
(println (.x p))            ;; → 10

;; Rust 扩展
;; (在单独的 Rust crate 中)
#[zio_export]
fn my_custom_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    Ok(Value::Integer(42))
}
```

---

## Phase 3: 宏与元编程 (2-3 周)

**目标**: 宏系统达到实用级成熟度 — 卫生宏、reader macro、compiler macro。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 3.1 | `syntax-rules` 卫生宏 | 🔴 P0 | `macros.rs` / [new] | +300 | 无 |
| 3.2 | Reader macro | 🟡 P1 | `reader.rs` | +80 | 1.2 |
| 3.3 | Compiler macro | 🟡 P1 | `macros.rs` | +100 | 3.1 |
| 3.4 | Code walker 基础 | 🟡 P2 | [new] `walker.rs` | +150 | 无 |

### 交付标准

```lisp
;; 卫生宏（不意外捕获变量）
(defmacro swap! [a b]
  `(let [tmp ~a]
     (set! ~a ~b)
     (set! ~b tmp)))
;; tmp 不会被外部同名变量干扰

;; Reader macro
(set-reader-macro! #\@
  (fn [stream char]
    `(deref ~(read stream))))   ;; @x → (deref x)

;; Compiler macro
(define-compiler-macro + [& args]
  `(fast-plus ~@args))
```

---

## Phase 4: 标准库 + 并发 (2-3 周)

**目标**: 可以写实际的多线程、网络应用。包管理系统可用。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 4.1 | 核心 .zio 标准库 | 🔴 P0 | [new] `stdlib/` | +500 Zio | 无 |
| 4.2 | 懒序列 | 🔴 P0 | `eval.rs` + stdlib | +150 | 无 |
| 4.3 | Future/Promise | 🔴 P0 | `builtins.rs`, `value.rs` | +200 | 无 |
| 4.4 | Channel CSP | 🟡 P1 | [new] `builtins/chan.rs` | +250 | 4.3 |
| 4.5 | Async/await macro | 🟡 P1 | [new] `special/async.rs` | +200 | 3.1 |
| 4.6 | JSON 序列化 | 🟡 P1 | `builtins.rs` | +80 | 无 |
| 4.7 | Test 框架 | 🟡 P2 | `stdlib/test.zio` | +200 Zio | 4.1 |
| 4.8 | 包管理器 `zio install` | 🔴 P0 | [new] `cli/pkg.rs` | +300 | 无 |

### 交付标准

```lisp
;; 懒序列
(def fibs (lazy-cat [0 1] (map + fibs (rest fibs))))
(take 10 fibs)          ;; → (0 1 1 2 3 5 8 13 21 34)

;; 并发
(def f (future (heavy-computation)))
(println @f)            ;; 阻塞等待

;; 包管理
;; $ zio install zio-json
(require :zio.json)
(zio.json/parse "{\"a\": 1}")
```

---

## Phase 5: CLOS + 多方法 (2-3 周)

**目标**: 达到 Common Lisp 的对象系统水平。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 5.1 | Generic function 分派 | 🔴 P0 | [new] `special/gf.rs` | +250 | 无 |
| 5.2 | Defmethod 定义 | 🔴 P0 | 同上 | +100 | 5.1 |
| 5.3 | :before/:after/:around | 🟡 P1 | 同上 | +150 | 5.2 |
| 5.4 | Condition system | 🟡 P1 | [new] `special/condition.rs` | +300 | 无 |

### 交付标准

```lisp
(defgeneric draw (shape))

(defmethod draw ((rect Rectangle))
  (println "Drawing a rectangle"))

(defmethod draw ((circle Circle))
  (println "Drawing a circle"))

(defmethod draw :before ((rect Rectangle))
  (println "About to draw rectangle"))

(draw (Rectangle 10 20))   ;; → "About to draw rectangle"
                           ;; → "Drawing a rectangle"
```

---

## Phase 6: LLM/Native (3-4 周)

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 6.1 | 向量原语 + 嵌入 | 🔴 P0 | `builtins.rs` + dep | +200 | 无 |
| 6.2 | LLM API 封装 | 🔴 P0 | `builtins.rs` | +300 | 无 |
| 6.3 | Prompt DSL | 🟡 P1 | `stdlib/prompt.zio` | +150 Zio | 4.1 |
| 6.4 | Agent 框架 | 🟡 P1 | `stdlib/agent.zio` | +300 Zio | 6.2 |
| 6.5 | Baseline JIT (Cranelift) | 🔴 P0 | [new] `zio-compiler/` | +500 | 无 |

### 交付标准

```lisp
;; LLM 调用
(llm/eval :model "gpt-4o" :prompt "Translate to French: hello")
;; → "Bonjour"

;; Agent
(def agent (agent "assistant"
               "You help users solve problems"
               [calculator file-reader]))

(agent/run agent "calculate 2^10 and save to log.txt")
```

---

## Phase 7: 生产化 (持续)

| # | 任务 | 优先级 | 估算 | 前提 |
|---|------|--------|------|------|
| 7.1 | LSP Server | 🟡 P1 | 2-3 周 | Phase 1-4 |
| 7.2 | Debugger (DAP) | 🟡 P2 | 2-3 周 | Phase 1-4 |
| 7.3 | WASM 编译 | 🟡 P2 | 1-2 周 | Phase 6.5 |
| 7.4 | Profiler | 🟡 P3 | 1-2 周 | Phase 2-4 |
| 7.5 | 文档生成 (doc) | 🟡 P2 | 1 周 | Phase 4.1 |

---

## 两个工程路标

### Datalog 数据库 (Phase 3-4 并行启动)

| 里程碑 | 时间 | 交付物 |
|--------|------|--------|
| P0: 核心数据结构 | Phase 3 W1 | `Datom` + `Database` + 索引 |
| P1: 单模式扫描 | Phase 3 W1 | `(q '[:find ?n :where [?e :name ?n]] db)` |
| P2: Hash join | Phase 3 W2 | 多模式连接查询 |
| P3: 事务系统 | Phase 4 W1 | add/retract + tx-id |
| P4: 时间旅行 | Phase 4 W2 | `as-of` / `since` |
| P5: 宏层 API | Phase 4 W2 | `q` defmacro + query composition |
| **MVP** | **Phase 4** | 内存 Datalog, 可查询, 可时间旅行 |

### 自学习模型框架 (Phase 4-5 启动)

| 里程碑 | 时间 | 交付物 |
|--------|------|--------|
| P0: Tensor 运算 | Phase 4 W2 | add/mul/matmul NativeFn |
| P1: 自动微分 | Phase 5 W1 | 反向模式 AD, Wengert list |
| P2: Layer API | Phase 5 W2 | dense/dropout/relu/softmax |
| P3: 训练循环 | Phase 5 W3 | SGD/Adam, loss 计算 |
| P4: Sexp 程序变换 | Phase 5 W3 | `generate-variants` |
| P5: 自改进循环 | Phase 6 W1 | 架构搜索 |
| **MVP** | **Phase 6** | MNIST 级自优化训练 |

---

## 依赖图（跨 Phase 的关键路径）

```
Phase 1 ─────── Span ───────→ Phase 1.2 Reader 扩展 ─────────→ Phase 3 Reader macro
                                │
                                └──→ Phase 1.3 macroexpand ─→ Phase 3 卫生宏
                                                                  │
                     Phase 4 懒序列 ←──────────────────────────────┘
                       │
                       ├──→ Phase 4.3 Future/Promise
                       │       │
                       │       └──→ Phase 4.4 Channel
                       │
                       ├──→ Phase 5 Generic function ←─── Phase 1 TCO
                       │
                       └──→ Phase 6 LLM eval

Phase 2.1 FFI ──────→ Phase 2.2 #[zio_export] ─────────→ Phase 6 JIT
                       │
                       ├──→ Phase 4.8 包管理器
                       │
                       └──→ Phase 2.5 struct ──────────→ Phase 5 CLOS

Phase 6.1 向量原语 ──→ Phase 6.2 LLM eval ─────────────→ Phase 6.4 Agent

Datalog P0 ───────────→ Datalog P2 Join ────────────────→ Datalog MVP
AutoML P0 ────────────→ AutoML P1 AD ──────────────────→ AutoML MVP
```

---

## 风险与缓解

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| **JIT 复杂度超预期** | 高 | 延迟 Phase 6 | Baseline 用 Cranelift 的 simple jit；不做优化 |
| **FFI 安全性问题** | 中 | 安全审计 | 默认沙箱 FFI 调用；unsafe 只在 ffi.rs |
| **im crate 性能瓶颈** | 中 | 大数据量慢 | 提供 `mutable-vec` 逃生口；可切换到 `imbl` |
| **宏系统设计分歧** | 低 | Code churn | 先 defmacro 两三年，hygiene 通过 `syntax-rules` 加 |
| **工程疲劳** | 中 | 进度拖慢 | 每个 Phase 有 clear deliverable + 可验证的 test |
| **LLM API 依赖不稳定** | 低 | 测试难以自动化 | 设计时用 trait 抽象后端；mock 优先 |

---

## 附录：快速启动建议

```bash
# 如果你想参与 Phase 1
cd zio
grep -rn "todo!" core/src/ reader/src/ cli/src/
# 当前 todo 项: macroexpand, span 集成, 更多 reader 语法

# 如果你想参与 Datalog
cd zio
# 从 Datom struct 开始 — 不依赖 Zio 运行时
# 在单独的 crate 或 core 内实验都可以
cargo new --lib zio-datalog

# 如果你想参与 AutoML
cd zio
# 先让 Tensor NativeFn 可调用
# (tensor/add [1 2] [3 4]) → [4 6]
# 这需要一点点 value.rs 扩展和 builtins.rs 注册
```

---

*此路线图为活文档 — 随工程进展不断更新。*