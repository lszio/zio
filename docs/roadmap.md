# Zio 路线图 — v0.2 → v1.0

> 2026-10-11 当前主线：语义独立的通用 Lisp、元循环/自举与运行时镜像，
> 最终以全套开发发行和可裁剪应用产物交付。
>
> 当前目标以[语言与工具链规格](superpowers/specs/2026-10-11-zio-language-toolchain-design.md)为准，
> 工作包、依赖和验收以[主计划](superpowers/plans/2026-10-11-zio-language-toolchain.md)为准。
> 下方旧 Phase 保留历史索引与证据，不是当前排期或本轮实跑结果。

---

## 概要

**当前基础**：已有 Rust Reader/AST evaluator、Experimental ZOS、字节码运行时、
Zio 编译器、自举对照测试、LSP 与 Tree-sitter/编辑器资源。可检查来源见
[架构 §3.3](zio-architecture.md#33-工具链当前来源与规划验收)；存在源码不等于本轮验收，
也不等于镜像或完整发行已经交付；L01 元循环与 L02 首轮便携编译器自举已验收，状态与实跑证据见主计划和语言参考。

**目标**：Zio 语义不依附 Rust trait；Rust 提供引导与机制，Zio 表达 reader、
eval/apply、展开、分析与编译。开发发行一次配齐解释器、编译器、LSP、语法/编辑器资源、
WASM 与文档；最小应用产物不强制带开发工具或未用引擎。

**排期依据**：按合同依赖与真实验收推进。旧版本号、估时、LOC 和测试数量
不作为当前交付承诺；测试用于证明行为，不以凑数量定义完成。

---

## 架构原则

1. **语义独立，保持四层目录** — `langs/` 持语言与工具链，`libs/` 持 Zio 库，
   `contribs/` 持可信宿主/引擎/工具适配，`apps/` 持应用。ZOS 保留核心，
   Numa/Rill/Loom 与独立项目 Groiz 只消费语言，不反向定义语义。
2. **先做对再做快** — 复用已有 AST、bytecode 和 Zio compiler；
   元循环/编译行为逐项对照，优化 ZIR/JIT 与机器码 AOT 不作为首轮完成前提。
3. **语言双主线与应用分开** — 元循环求值器/自举和镜像/导出分别验收，
   汇合到完整发行；Groiz（独立项目）的应用演化另循其自身计划，不阻塞语言发行。

## 当前目录切换与后续实现（2026-10-07）

该日期标题保留原引用：2026-10-07 的[目录计划](superpowers/plans/2026-10-07-language-first-layout.md)
记录首轮物理布局切换及当时实跑，不是当前工作包。四层目录保持，不另起迁移布局。
独立项目 Groiz 的存储、调度、评价和治理等 Rust 业务不在本仓库；可信安全和后端
机制保留必要 Rust 实现。`libs/rill/cli.zio` 已有 CLI 解析组件，独立发行与完整合同另行验收。
编译器、LSP、Tree-sitter 已有来源，不再把它们写成完全不存在。

### 当前双主线与汇合验收（2026-10-11）

| 主线 | 当前起点 | 目标与验收 |
|---|---|---|
| A：元循环求值器与自举编译器 | Rust AST、`bytecode.rs`、`compiler.zio`、`self_hosting.rs` | 固定语言/对象/模块与源码位置合同；Zio eval/apply 与 AST/bytecode 行为对照；编译器产物编译自身、规范化固定点与目标语义覆盖 |
| B：运行时镜像与导出 | 现有值、闭包与模块机制；模块序列化不等于镜像 | 对象图保存/恢复保留身份、共享、循环及闭包；外部资源明确重建，否则拒绝；恢复权限重新授予；分别验收 bin、浏览器 WASM 与 WASI |
| 汇合：引擎与完整开发发行 | 现有宿主接口、工具/语法资源、浏览器入口 | 外部解释/编译引擎库明确源码入口与制品合同；解释器、编译器、LSP、语法资源、WASM、文档一起交付，干净安装可用，不依赖构建机源码路径 |

以源码为输入，四类产物为完整开发发行版、编译模块（便携字节码）、运行时镜像、
应用产物；native bin、WASM-browser、WASM-WASI 是应用产物子类型。
首轮 bin 是 bytecode/初始镜像 + runtime 打包，不是机器码 AOT；完整开发发行与最小
应用产物分开。`inline` 是外语代码入口，不必然 eval；解释执行或编译制品由引擎
库合同决定。当前 JS bridge 为 WASM-only，`js` dotted resolver 为 interpreter-only，
native CLI 无 JS，不把它们当作通用外语引擎已交付。

依赖方向：先固定语义与运行机制，A/B 可按共同合同分别推进；镜像恢复须有
明确的代码/模块绑定和资源能力合同；导出消费稳定制品与恢复入口；完整发行再
验收工具、引擎、平台与安装组合。详细工作包只维护在
[主计划](superpowers/plans/2026-10-11-zio-language-toolchain.md)，本页不复制第二套排期。

### 历史 Phase 与计划的读取方式

下面 Phase 1–7、应用蓝图、依赖图和风险矩阵保留原标题及编号，避免破坏引用。
其旧估时、LOC、测试数目标、目录和 API 示例均为**历史规划快照**，不是当前
实现状态、待办优先级或承诺；历史勾选/命令与测量仅在当时任务范围内成立。
本轮未重跑其中任何命令。未实现 API 示例不能直接作为当前可运行用法。

2026-10-07 [目录计划](superpowers/plans/2026-10-07-language-first-layout.md)作为历史证据索引保留；目标、物理归属和当前
依赖以前述新规格、新计划与[架构](zio-architecture.md)为准，不按旧目录重建 Rust 库。


---

## Phase 1: 核心稳定化（1-2 周）

**目标**：Zio 成为可靠的 Lisp 内核——错误消息可用、reader 完整、TCO 正确。

| # | 任务 | 优先级 | 文件 | 估算 LOC | 前提 |
|---|------|--------|------|----------|------|
| 1.1 | Span 嵌入 Sexp | P0 | `sexp.rs`, `reader.rs`, `eval.rs`, `error.rs` | +80, -40 | 无 |
| 1.2 | Reader 扩展 (`#()` `#{}` `#\c` `,@`) | P0 | `reader/src/reader.rs` | +100 | 1.1 |
| 1.3 | 通用 TCO（所有尾位置） | P1 | `eval.rs`, `special/control.rs` | +30, -5 | 无 |
| 1.4 | `macroexpand` 实现 | P1 | `builtins/`, `macros.rs` | +30 | 无 |
| 1.5 | EvalEngine 拆分（ADR-005） | P1 | `context.rs`, `eval.rs`, `special/*.rs` | +60 | 无 |
| 1.6 | IoHost trait（ADR-011） | P1 | [new] `io.rs`, `builtins/` | +80 | 1.5 |
| 1.7 | 错误类型扩展 + 更多变体 | P1 | `error.rs` | +50 | 1.1 |
| 1.8 | 测试覆盖提升（目标由当前生成基线提升至 200+） | P1 | 全模块 | +200 | 1.1-1.4 |

### 交付标准

```
cargo test → 200+ tests passing
cargo clippy → warning-free target

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
| 2.9 | Package 系统 | P1 | `zos/package.rs`, `module.rs` | +200 | 2.2 |
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
| 4.3 | Value::Buffer + 字节向量 | P1 | `value.rs`, `builtins/` | +80 | 无 |
| 4.4 | File I/O builtins（通过 IoHost） | P1 | `builtins/` | +120 | 1.6 |
| 4.5 | `(defstruct ...)` 结构体 | P1 | [new] `special/struct.rs` | +150 | 无 |
| 4.6 | 核心 .zio 标准库 | P1 | [new] `stdlib/` | +500 Zio | 无 |
| 4.7 | 懒序列 | P1 | `eval.rs` + stdlib | +150 | 无 |
| 4.8 | Future/Promise | P1 | `builtins/`, `value.rs` | +200 | 无 |
| 4.9 | Channel CSP | P2 | [new] `builtins/chan.rs` | +250 | 4.8 |
| 4.10 | JSON 序列化 | P2 | `builtins/` | +80 | 4.6 |
| 4.11 | 包管理器 `zio install` | P0 | `tools/src/pkg.rs` | +300 | 1.5 |

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

## Phase 5: 扩展库生态（Planned，2-3 周）

**目标**：通过 Macro + MOP 构建官方扩展库。

| # | 任务 | 文件 | 估算 LOC | 前提 |
|---|------|------|----------|------|
| 5.1 | `zio-persistent`: 持久化 Vector/Map/Set | `libs/std/persistent.zio` | +300 | 3.4 |
| 5.2 | `zio-entity`: Entity 模型 | `libs/std/entity.zio` | +200 | 2.7, 3.6 |
| 5.3 | `zio-protocol`: Protocol 系统 | `libs/std/protocol.zio` | +250 | 3.5, 3.6 |
| 5.4 | Zio 编辑器原型（应用自托管） | [new] `tools/zide/` | +1000 Zio | 无 |

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

## Phase 6: 高级生态（Planned，3-4 周）

**目标**：Datalog、Agent、AI 原型。Baseline JIT 可行性验证。

| # | 任务 | 文件 | 估算 LOC | 前提 |
|---|------|------|----------|------|
| 6.1 | `zio-datalog` MVP | `libs/std/datalog.zio` | +600 | 5.1 |
| 6.2 | `zio-agent` 原型 | `libs/loom/agent/*.zio` | +300 | 4.1, 4.8 |
| 6.3 | `zio-llm` API 封装 | `libs/loom/llm.zio` | +300 | 4.1 |
| 6.4 | JIT 可行性验证（Cranelift） | `tools/` feature | +300 | 无 |
| 6.5 | 向量原语 + 嵌入 | `builtins/` + dep | +200 | 4.1 |

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

## Phase 6-C: 程序合成模块（L1-L3 已实现，L4 Planned）


**目标**：把同象性学习器 MVP 升级为自学习闭环——学习机器为提议器
（LLM 主线，遗传算子/RL/神经网络各有明确扩展位）、eval 为裁判、
语言化记忆做经验检索（结构/行为指纹/可选向量三索引）。独立模块演进，
含论文与汇报交付。

> 详细计划（模块边界、L1-L4 阶段、验收标准、论文与汇报规划）见
> [程序合成模块计划](synthesis-plan.md)；本节细化并取代 Phase 6 的
> 6.3 / 6.5 中与 LLM/embedding 相关的条目。

| 阶段 | 内容 | 交付物 | 前提 |
|------|------|--------|------|
| L1 ✅ | `LlmHost` / `EmbedHost` 宿主协议（外部 attach）+ Mock 录制/回放 | 新 crate `zio-ai` + 合同测试 | ADR-011/016 |
| L2 ✅ | `libs/loom/proposer.zio`（纯 Zio）：提示模板/解析/重试 | Mock 下端到端提议器 | L1 |
| L3 ✅ | 学习循环泛化：proposer 协议 + 代际/预算 + 闭世界白名单 + 错误隔离（遗传提议器可选） | 深度 3 求解 demo（枚举不可行 = 500 eval 预算内不可解，LLM 3 eval 解决） | L2；`libs/learning/learn.zio` |
| L4 | `libs/learning/memory.zio` 三索引经验库 + 反统一蒸馏 + 环境吸收 + 自修复（vector.zio 语义桥、bandit、玩具 NN 可选） | 调用递减曲线 + 首个蒸馏宏 + DSL 收缩报表 | L3 |

**论文/汇报**：工作坊论文（L3 后）→ 完整论文（L4 后）；内部里程碑
汇报 ×4 + 外部技术分享。规划见 synthesis-plan 第 5/6/7 节。

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
| 7.6 | Zio 编辑器 LSP 集成（应用自托管） | 持续 | 5.4, 7.1 |

---

## 应用蓝图（Planned）

以下应用是规划中的扩展库，独立于核心路线图；它们不是当前已实现
能力。Persistent collection、Datalog、JIT 及应用能力的权威状态见
[特性矩阵](feature-matrix.md)。

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

### Harness 与 agent 逻辑

旧 `zio-agent` 框架蓝图由 H00/G01/G02 收敛：公共 harness 持有模型与工具
会话、预算/取消，Zio agent 逻辑实际驱动步骤；产品侧的学习与版本治理属于
消费它的应用（Groiz 为独立项目，不在本仓库）。
`libs/loom/agent.zio` 的 canned echo 已由真实 `agent-run` 循环替换：它决定重试
与停止，宿主通过 `harness-complete`/`harness-execute-zio` 提供模型调用与受限
执行（宿主绑定由嵌入应用实现）。历史 G02 live 验收（`omp acp` + 真实模型：
首轮生成即产出可复算的程序，失败首轮经诊断后在第二轮修订为 candidate）
的宿主侧制品随宿主 crate 迁往独立项目 Groiz，不在本仓库。
ACP 客户端/教师适配与服务端分别验收；不另起重叠框架。

### Zio 编写的编辑器（zide，应用自托管，不等同编译器自举）

| 里程碑 | 时间 | 交付物 |
|--------|------|--------|
| P0: 语法高亮 REPL | Phase 5 | 在 Zio REPL 中嵌入编辑器 |
| P1: 文件编辑器 | Phase 6 | 打开、编辑、保存 .zio 文件 |
| P2: 括号匹配 | Phase 6 | Sexp 感知的括号匹配 |
| P3: LSP 集成 | Phase 7 | 补全 + 跳转定义（需要 LSP） |
| **应用原型** | **旧 Phase 7 目标** | Zio 编辑器可用；工具链自举另由 T01–T03 验收 |

---

## 依赖图

下面仅保留旧 Phase 的历史依赖示意。当前语言双主线以本页前部与新主计划
为准；独立应用（Groiz，独立项目）的验收不等待完整 VM/JIT、编辑器或镜像。

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
