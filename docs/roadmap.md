# Zio 路线图 — v0.2 → v1.0

> 从 103 tests 的 Lisp 内核 + ZOS 规范到自举的通用语言

---

## 概要

**当前**：v0.2 — 架构重构完成。0 thread-local 全局变量。103 tests passing。ZOS 规范已发布。

**目标**：v1.0 — 自举工具链、嵌入式友好、ZOS 对象系统完整、扩展库生态 MVP。

**时间线**：预计 8-10 个月，按功能 Phase 而非严格时间线组织。

---

## 架构原则

两条核心原则驱动路线图：

1. **核心最小，其余是库** — Datalog、Agent、自学习全部作为扩展库，不进入 Core
2. **先做对再做快** — AST 解释器 + ZOS 运行时先验证语义，JIT/ZIR 只在验证性能瓶颈后引入

---

## Phase 1: 核心稳定化（1-2 周）

**目标**：Zio 成为可靠的 Lisp 内核——错误消息可用、reader 完整、TCO 正确。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 1.1 | Span 嵌入 Sexp | P0 | `sexp.rs`, `reader.rs`, `eval.rs`, `error.rs` | +80, -40 | 无 |
| 1.2 | Reader 扩展 (`#()` `#{}` `#\c` `,@`) | P0 | `reader/src/reader.rs` | +100 | 1.1 |
| 1.3 | 通用 TCO（所有尾位置） | P1 | `eval.rs`, `special/control.rs` | +30, -5 | 无 |
| 1.4 | `macroexpand` 实现 | P1 | `builtins.rs`, `macros.rs` | +30 | 无 |
| 1.5 | EvalEngine 拆分（ADR-005） | P1 | `context.rs`, `eval.rs`, `special/*.rs` | +60 | 无 |
| 1.6 | IoHost trait（ADR-011） | P1 | [new] `io.rs`, `builtins.rs` | +80 | 1.5 |
| 1.7 | 错误类型扩展 + 更多变体 | P1 | `error.rs` | +50 | 1.1 |
| 1.8 | 测试覆盖提升（103 → 200+） | P1 | 全模块 | +200 | 1.1-1.4 |

### 交付标准

```
cargo test → 200+ tests passing
cargo clippy → 0 warnings

;; 带行号的错误消息
(car 1) → "Not a list (car expects a list)"
          "  └─ repl:1:6"
          "     (car 1)"
          "          ^"

;; 全 TCO：尾递归不爆栈
(defn even? [n] (if (zero? n) true (odd? (dec n))))
(defn odd? [n] (if (zero? n) false (even? (dec n))))
(even? 100000) → true     ;; 不爆栈
```

---

## Phase 2: ZOS Phase 1（2-3 周）

**目标**：ZOS 最小对象系统可用——`defclass`、`defgeneric`、`defmethod`、Package、简化 Condition。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 2.1 | `Value::Object` + `ZosObject` trait | P0 | `zos/object.rs`, `value.rs` | +80 | 1.5 |
| 2.2 | Built-in Class 注册表 + `class-of` | P0 | `zos/class.rs` | +120 | 2.1 |
| 2.3 | `defclass` 特殊形式 | P0 | `zos/class.rs`, `special/zos.rs` | +150 | 2.2 |
| 2.4 | Slot 定义 + `slot-value` / `(setf slot-value)` | P0 | `zos/slot.rs` | +120 | 2.3 |
| 2.5 | Generic Function 结构 | P0 | `zos/gf.rs` | +100 | 2.2 |
| 2.6 | 4-参数 Dispatch Cache（ADR-008） | P0 | `zos/gf.rs` | +60 | 2.5 |
| 2.7 | `defgeneric` / `defmethod` 特殊形式 | P0 | `zos/gf.rs`, `zos/method.rs`, `special/zos.rs` | +120 | 2.6 |
| 2.8 | Method Combination 基础（primary, :before, :after, :around, call-next-method） | P1 | `zos/method.rs` | +120 | 2.7 |
| 2.9 | Package 系统 | P1 | `zos/package.rs`, `symbol.rs` | +200 | 2.2 |
| 2.10 | Condition System 简化版（ADR-007） | P1 | `zos/condition.rs`, `eval.rs` | +250 | 1.5 |

### 交付标准

```lisp
;; Class + Slot
(defclass point ()
  ((x :initarg :x :accessor point-x)
   (y :initarg :y :accessor point-y)))

(def p (make-instance 'point :x 10 :y 20))
(point-x p)               ;; → 10

;; Generic Function + Method
(defgeneric draw (shape))

(defmethod draw ((p point))
  (println "Point at" (point-x p) (point-y p)))

(draw p)                  ;; → "Point at 10 20"

;; Package
(defpackage :zio.math
  (:export :pi :sin :cos))

;; Condition
(try
  (error "something went wrong")
  (catch "Not a list" msg)
  (handler 42))
```

---

## Phase 3: 卫生宏 + ZOS Phase 2（2-3 周）

**目标**：`syntax-rules` 公卫宏可用。ZOS 的 MOP、多分派、完整反射。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 3.1 | `syntax-rules` 卫生宏（ADR-010） | P0 | `macros.rs` | +300 | 无 |
| 3.2 | Reader macro | P1 | `reader.rs` | +80 | 3.1 |
| 3.3 | C3 线性化 + 多继承 | P1 | `zos/class.rs` | +100 | 2.3 |
| 3.4 | 多分派（3+ 参数完整） | P1 | `zos/gf.rs` | +120 | 2.7 |
| 3.5 | MetaClass 注册表 | P1 | `zos/mop.rs` | +80 | 2.2 |
| 3.6 | 完整反射 API | P2 | `zos/reflection.rs` | +150 | 3.5 |
| 3.7 | 测试覆盖提升（200 → 350+） | P1 | 全模块 | +300 | 所有 |

### 交付标准

```lisp
;; 卫生宏（不意外捕获变量）
(defmacro swap! [a b]
  (syntax-rules ()
    ((swap! a b)
     (let [tmp a]
       (set! a b)
       (set! b tmp)))))

;; 多分派
(defgeneric collide (a b))

(defmethod collide ((car car) (wall wall))
  "Car hits wall")

(defmethod collide ((car car) (truck truck))
  "Car bounces off truck")

(collide my-car brick-wall)   ;; → "Car hits wall"

;; 反射
(class-of my-car)             ;; → #<Class CAR>
(slot-definitions my-point)   ;; → ((X ...) (Y ...))
(methods #'draw)              ;; → (#<Method DRAW (POINT)>)
```

---

## Phase 4: 标准库 + 系统编程（2-4 周）

**目标**：能写程序。文件 I/O、FFI、懒序列、并发、包管理器。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 4.1 | FFI 原型（C ABI） | P0 | [new] `ffi.rs` | +200 | 1.5 |
| 4.2 | `#[zio_export]` proc-macro | P0 | [new] `zio-macros/` crate | +150 | 4.1 |
| 4.3 | Value::Buffer + 字节向量 | P1 | `value.rs`, `builtins.rs` | +80 | 无 |
| 4.4 | File I/O builtins（通过 IoHost） | P1 | `builtins.rs` | +120 | 1.6 |
| 4.5 | `(defstruct ...)` 结构体 | P1 | [new] `special/struct.rs` | +150 | 无 |
| 4.6 | 核心 .zio 标准库 | P1 | [new] `stdlib/` | +500 Zio | 无 |
| 4.7 | 懒序列 | P1 | `eval.rs` + stdlib | +150 | 无 |
| 4.8 | Future/Promise | P1 | `builtins.rs`, `value.rs` | +200 | 无 |
| 4.9 | Channel CSP | P2 | [new] `builtins/chan.rs` | +250 | 4.8 |
| 4.10 | JSON 序列化 | P2 | `builtins.rs` | +80 | 4.6 |
| 4.11 | 包管理器 `zio install` | P0 | [new] `cli/pkg.rs` | +300 | 1.5 |

### 交付标准

```lisp
;; FFI
(def sqrt (ffi/c "libm.so.6" "sqrt" f64 -> f64))
(println (sqrt 25.0))     ;; → 5.0

;; 文件操作
(def data (slurp "/etc/hostname"))
(spit "/tmp/out.txt" data)

;; 懒序列
(def fibs (lazy-cat [0 1] (map + fibs (rest fibs))))
(take 10 fibs)            ;; → (0 1 1 2 3 5 8 13 21 34)

;; 并发
(def f (future (heavy-computation)))
(println @f)              ;; 阻塞等待

;; 包管理器
; $ zio install zio-json
(require :zio.json)
(zio.json/parse "{\"a\": 1}")
```

---

## Phase 5: 扩展库生态（2-3 周）

**目标**：通过 Macro + MOP 构建官方扩展库。

| # | 任务 | 文件 | 估算 LOC | 前提 |
|---|------|------|----------|------|
| 5.1 | `zio-persistent`: 持久化 Vector/Map/Set | [new] `lib/zio-persistent/` | +300 | 3.4 |
| 5.2 | `zio-entity`: Entity 模型 | [new] `lib/zio-entity/` | +200 | 2.7, 3.6 |
| 5.3 | `zio-protocol`: Protocol 系统 | [new] `lib/zio-protocol/` | +250 | 3.5, 3.6 |
| 5.4 | 自举编辑器原型 | [new] `tools/zide/` | +1000 Zio | 无 |

### 交付标准

```lisp
;; zio-persistent: Clojure 风格集合
(require :zio.persistent)
(def m (assoc {} :a 1 :b 2))
(get m :a)                ;; → 1
(assoc m :c 3)            ;; → {:a 1 :b 2 :c 3}（原 m 不变）

;; zio-entity: 带身份的对象
(require :zio.entity)
(defentity user [name email])
(def u (make-entity 'user :name "Alice" :email "alice@x.com"))
(entity-id u)             ;; → uuid

;; zio-protocol: 类似 Clojure protocols
(defprotocol Drawable
  (draw [this]))

(extend-type point Drawable
  (draw [this]
    (println "Point at" (slot-value this 'x) (slot-value this 'y))))
```

---

## Phase 6: 高级生态（3-4 周）

**目标**：Datalog、Agent、AI 原型。Baseline JIT 可行性验证。

| # | 任务 | 文件 | 估算 LOC | 前提 |
|---|------|------|----------|------|
| 6.1 | `zio-datalog` MVP | [new] `lib/zio-datalog/` | +600 | 5.1 |
| 6.2 | `zio-agent` 原型 | [new] `lib/zio-agent/` | +300 | 4.1, 4.8 |
| 6.3 | `zio-llm` API 封装 | [new] `lib/zio-llm/` | +300 | 4.1 |
| 6.4 | JIT 可行性验证（Cranelift） | [new] `zio-compiler/` prototype | +300 | 无 |
| 6.5 | 向量原语 + 嵌入 | `builtins.rs` + dep | +200 | 4.1 |

### 交付标准

```lisp
;; Datalog
(require :zio.datalog)
(q '[:find ?name
     :where [?e :person/name ?name]
            [?e :person/age ?age]
            [(> ?age 30)]]
   db)                   ;; → #{["Alice"] ["Bob"]}

;; Agent
(require :zio.agent)
(def agent (agent "assistant" "help user" [calculator file-reader]))
(agent/run agent "calculate 2^10 and save to log.txt")

;; LLM
(require :zio.llm)
(llm/completion :model "gpt-4" :prompt "Translate to French: hello")
```

---

## Phase 7: 生产化（持续）

**目标**：工具链完备、跨平台、性能可预期。

| # | 任务 | 估算 | 前提 |
|---|------|------|------|
| 7.1 | LSP Server | 2-3 周 | Phase 1-4 |
| 7.2 | Debugger（DAP） | 2-3 周 | Phase 1-4 |
| 7.3 | WASM 编译 | 1-2 周 | Phase 6.4 |
| 7.4 | Profiler | 1-2 周 | Phase 2-4 |
| 7.5 | 文档生成（doc） | 1 周 | Phase 4.6 |
| 7.6 | 自举编辑器 LSP 集成 | 持续 | 5.4, 7.1 |

---

## 应用蓝图

以下应用是扩展库，独立于核心路线图，可并行推进。

### zio-datalog 数据库

| 里程碑 | 时间 | 交付物 |
|--------|------|--------|
| P0: 核心数据结构 | Phase 4 W1 | Datom + Database + 索引 |
| P1: 单模式扫描 | Phase 4 W1 | `(q '[:find ?n :where [?e :name ?n]] db)` |
| P2: Hash join | Phase 4 W2 | 多模式连接查询 |
| P3: 事务系统 | Phase 4 W2 | add/retract + tx-id |
| P4: 时间旅行 | Phase 5 W1 | `as-of` / `since` |
| P5: 宏层 API | Phase 5 W1 | `q` defmacro + query composition |
| **MVP** | **Phase 5** | 内存 Datalog, 可查询, 可时间旅行 |

### zio-agent 框架

| 里程碑 | 时间 | 交付物 |
|--------|------|--------|
| P0: Tool 注册表 | Phase 5 W2 | ToolSpec + tool-use protocol |
| P1: Agent 循环 | Phase 5 W2 | eval-with-tools + multi-turn |
| P2: Memory | Phase 6 W1 | conversation history + persistence |
| P3: Tool 链 | Phase 6 W1 | 组合多个 tool 的 workflow |
| **MVP** | **Phase 6** | 可用的 agent 编排框架 |

### 自举编辑器（zide）

| 里程碑 | 时间 | 交付物 |
|--------|------|--------|
| P0: 语法高亮 REPL | Phase 5 | 在 Zio REPL 中嵌入编辑器 |
| P1: 文件编辑器 | Phase 6 | 打开、编辑、保存 .zio 文件 |
| P2: 括号匹配 | Phase 6 | Sexp 感知的括号匹配 |
| P3: LSP 集成 | Phase 7 | 补全 + 跳转定义（需要 LSP） |
| **MVP** | **Phase 7** | 自举编辑器可用 |

---

## 依赖图

```
Phase 1 ──── Span ───────────────────→ Phase 1.2 Reader 扩展 ───→ Phase 3 Reader Macro
              │
              │── TCO ────→ Phase 2 GF dispatch（尾调用多分派）
              │
              └── EvalEngine Split ──→ Phase 2 ZOS Object
                                          │
                                          ├──→ Phase 2.1-2.8: Class/GF/Method
                                          │       │
                                          │       └──→ Phase 3.3-3.6: MOP / Multi-dispatch
                                          │
                                          ├──→ Phase 2.9 Package
                                          │
                                          └──→ Phase 2.10 Condition

Phase 3 ──── syntax-rules ───────────→ Phase 4 懒序列（宏实现）
              │                          │
              │                          ├──→ Phase 4 Future/Promise
              │                          └──→ Phase 5 zio-persistent
              │
              └───────────→ Phase 5 zio-protocol（宏实现）

Phase 4 ──── FFI ────→ Phase 4.2 #[zio_export]
              │              │
              │              ├──→ Phase 4.11 包管理器
              │              └──→ Phase 6 JIT
              │
              ├── File I/O ──→ Phase 6 zio-datalog
              │
              └── Future ────→ Phase 6 zio-agent
                                   │
                                   └──→ Phase 6 LLM eval
```

---

## 风险矩阵

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| ZOS 的 Value::Object + dyn downcast 性能不可接受 | 中 | 高 | Phase 2 前做一个性能基准，若不可接受则用外部 crate 注册表代替 trait object |
| 卫生宏在 AST 解释器中实现复杂度高 | 中 | 中 | `syntax-rules` 模式匹配的算法确定后在 Phase 3 做 proc-macro 原型 |
| Condition System 完整实现受 Rust 控制流模型限制 | 中 | 中 | Phase 1 只做简化版，如果用户社区要求完整版再投入 |
| 包管理器标准未定义导致生态碎片 | 低 | 高 | Phase 4 前发布包规范草案，参考 Racket / ASDF / Cargo 的包布局 |
| 单线程 `RefCell` 在 Phase 4 多线程时成为阻塞项 | 中 | 中 | Phase 3 末做 `Send` / `Sync` 审计，确定 `Mutex` vs `arc-swap` 迁移路径 |
