# Zio 路线图 — v0.2 → v1.0

> 从可验证的 AST Lisp 内核 + experimental ZOS 子集到自举的通用语言
>
> 未交付的 Phase、交付标准和应用蓝图是规划目标；历史 Grove 表格保留
> 原任务交付证据，不代表新的 agent 产品主线完成。当前能力状态以
> [特性矩阵](feature-matrix.md)为准，生成的仓库数量见
> [项目状态](status.md)。

---

## 概要

**当前**：v0.2 — AST evaluator 可运行，ZOS class/generic dispatch 子集为
Experimental；可验证数量见[项目状态](status.md)。

**目标**：v1.0 — 自举工具链、嵌入式友好、ZOS 对象系统完整、扩展库生态 MVP。

**排期依据**：按合同依赖与真实验收推进，不沿用旧的 8–10 个月估算或测试数量目标作为交付承诺。

---

## 架构原则

三条原则驱动路线图：

1. **核心收敛，保留 ZOS** — 对象/类/泛型函数/方法/MOP 属于语言核心方向；官方独立库 Numa（计算）、Rill（CLI 组合）、Loom（Agent harness）与独立 Grove 应用不反向进入语言语义。
2. **先做对再做快** — AST 解释器先验证语义；JIT/ZIR/bytecode VM 当前均为 [Planned](feature-matrix.md)，不同时作为第一轮自举前提。
3. **工具链自举与应用演化分开** — Zio 编写展开/分析/编译器，Rust 持最小运行时和宿主；Grove 先验证可编码 agent、生成执行和人工审查升级。两条线互不阻塞，见 [ADR-019](adrs.md#adr-019-grove-独立应用与同像性逻辑演化)。

## 当前实现顺序（2026-10-05）

唯一开工清单是[统一实现计划](superpowers/plans/2026-10-05-zio-grove-convergence.md)，
共 18 个工作包；下面旧 Phase 1–7 保留为功能目录，不是必须重新按顺序执行。

| 主线 | 工作包 | 完成门槛 |
|------|--------|----------|
| 共享语言起点 | T00 | 数值、来源与复用装配合同；ZOS 保留核心，宿主默认拒绝 |
| 首条 Grove 产品 | G00 → G01/H00 → G02 → G03 → G04 → G05 | 先强制保护与恢复，真实 Zio agent/模型生成执行，再独立评价、人工发布与真实站点证据 |
| 工具链自举 | T01 → T02 → T03 | Zio 完整展开/分析/编译器、最小字节码后端、自编译与重复构建，独立于 Grove 完成 |
| Numa 与 Rill 官方独立库 | C00 → C01 → C02；I00 | 实际消费者驱动抽取，Numa 提供连续数组/矩阵/向量及后端接口与性能证据；Rill 提供参数/子命令/帮助、命令组合、终端 I/O 与退出状态，不承担语言求值 |
| 后续 Grove 扩展 | G06 → G07；G08 | ACP 客户端/教师后服务端，逐类模型能力，离开源码树的安装与权限验收 |

Numa、Rill、Loom 按需安装、独立版本发布，Grove 消费语言与三库；
Loom 的模型、工具、会话、预算、取消、provider 与 ACP 不持学习目标、
独立评价或发布治理。三库不先建空框架，Grove 也不复制解释器或 agent loop。
当前 `zio-ai`/`ai/` 是 Loom 的复用起点，`zio-cli`/`cli/` 是未来消费
Rill 的可执行宿主；当前 crate、目录、命令和已有函数名不变。
正式短名不是已确定的注册包 ID，未来规范目录与命名空间见[术语表](glossary.md)。
这张依赖表与三库新增能力都是 Planned，不是本次实现结果。

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
| 5.1 | `zio-persistent`: 持久化 Vector/Map/Set | `lib/zio/persistent.zio` | +300 | 3.4 |
| 5.2 | `zio-entity`: Entity 模型 | `lib/zio/entity.zio` | +200 | 2.7, 3.6 |
| 5.3 | `zio-protocol`: Protocol 系统 | `lib/zio/protocol.zio` | +250 | 3.5, 3.6 |
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
| 6.1 | `zio-datalog` MVP | `lib/zio/datalog.zio` | +600 | 5.1 |
| 6.2 | `zio-agent` 原型 | `lib/zio/agent/*.zio` | +300 | 4.1, 4.8 |
| 6.3 | `zio-llm` API 封装 | `lib/zio/llm.zio` | +300 | 4.1 |
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
| L2 ✅ | `lib/zio/proposer.zio`（纯 Zio）：提示模板/解析/重试 | Mock 下端到端提议器 | L1 |
| L3 ✅ | 学习循环泛化：proposer 协议 + 代际/预算 + 闭世界白名单 + 错误隔离（遗传提议器可选） | 深度 3 求解 demo（枚举不可行 = 500 eval 预算内不可解，LLM 3 eval 解决） | L2；`lib/zio/learn.zio` |
| L4 | `lib/zio/memory.zio` 三索引经验库 + 反统一蒸馏 + 环境吸收 + 自修复（vector.zio 语义桥、bandit、玩具 NN 可选） | 调用递减曲线 + 首个蒸馏宏 + DSL 收缩报表 | L3 |

**论文/汇报**：工作坊论文（L3 后）→ 完整论文（L4 后）；内部里程碑
汇报 ×4 + 外部技术分享。规划见 synthesis-plan 第 5/6/7 节。

### Grove 独立应用：新主线 Planned，历史 P1–P4 为组件基础

产品定位与当前优先级以[整体设计](self-learning-architecture.md)和 [ADR-019](adrs.md#adr-019-grove-独立应用与同像性逻辑演化)
为准，不再采用学习库优先、可选产品壳的交付顺序：

| 新阶段 | 状态 | 交付与验收 |
|---|---|---|
| G1 可编码 agent 与执行模块 | Planned | Zio 逻辑真实驱动 agent；真实 LLM 生成代码并经隔离执行，错误进入修订；执行记录固定程序/依赖与预算，越权或超时被拒 |
| G2 可审查逻辑演化与站点 | Planned | 反馈产生真实逻辑候选；展示代码差异、执行路径、独立回归与检查点；无人工批准不激活，批准只激活指定候选；在途调用固定旧版，冻结区不变；实时 HTML 与静态报告共享事实 |
| G3 教师与可追加模块 | Planned | ACP 客户端/教师适配在前、服务端在后；逐步接入向量/神经/LLM 模块；能力和恢复等级如实声明；独立安装不依赖构建机源码路径 |

先处理冻结参数/文件权限与恢复约束，再接真实 runner、反馈消费证据与持久
事件。公共 Numa 计算、Rill CLI 组合、Loom harness 能力按实际复用抽取，不先建通用学习框架。

具体新文件、现有修改点、失败行为检查、运行命令和权限合同集中在统一
实现计划 G00–G08；本页不再维护第二套细粒度实施清单。

下表保留[历史 P1–P4 交付计划](superpowers/plans/2026-10-02-self-learning.md)
的 W00–W17 任务记录与当时测量，未在本次文档修订中重跑。P1–P4 不重命名
L1–L4，也不替代 G1–G3；demo 真实训练不代表服务队列或 agent 产品主线完成。

下表中的 `zio-ai`、`zio-cli`、`lib/zio/*` 和历史 crate/测试命令保留为
当时及当前源码事实，不批量替换为未实现的 Numa、Rill 或 Loom 包命令。

| 阶段 | 状态 | 已验证能力 | 验证命令 |
|---|---|---|---|
| P1 W00 验收合同 | ✅ | 几何图像 + 传感器 XOR 任务，按 scene 分组切分（泄漏 0），缺失模态必须 abstain，holdout 容器不存标签字节 | `python examples/self-learning/generate.py --self-check` |
| P1 W01 宿主存储 | ✅ | `grove` crate：版本化记录 + 稳定错误类、内容寻址制品（摘要校验、临时文件后提交）、SQLite 事务（角色门控、信号幂等回执、head/发布乐观版本）；SIGKILL 写进程后无可见半成品 | `cargo test -p grove --test store_contract`（22） |
| P1 W02 反馈生命周期 | ✅ | 纯 Zio 反馈政策（字段作用域、人工优先于教师、冲突隔离、abstain 一等、冻结数据视图）+ 宿主侧信号生命周期 | `cargo test -p grove --test feedback_contract`（12） |
| P1 W03 教师接口 | ✅ | 教师能力声明（模态/软输出词表对齐/许可/数据保留）、提议程序仅作候选、HTTP 适配器与传输限制 | `cargo test -p zio-ai --test teacher_contract --features http`（16） |
| 本地教师（torch CPU） | ✅ | 真实反向传播（loss 0.696 → 0.0003，val 准确率 1.000），HTTP 服务并经 Rust 教师宿主查询 | `GROVE_TEACHER_WEIGHTS=… cargo test -p grove --test teacher_local` |
| P2 W04 worker 与隔离 | ✅ | 许可算子图（执行前结构校验）+ NDJSON 控制协议 + `unshare -Urn` 命名空间隔离（网络探测：外部可达/内部阻断）、rlimits、进程组回收、不支持隔离时 fail closed | `cargo test -p grove --test worker_contract`（10）；`python -m unittest discover -s workers/torch/tests -p test_worker.py`（13） |
| P2 W05 联合学习 driver | ✅ | 模型描述即 Zio 数据、结构候选即改写（线性→非线性融合）；W00 任务实测：线性基线 val 准确率 0.531（XOR 卡死），联合候选 1.000，提升 +46.9pp（冻结门槛 +15pp） | `cargo test -p grove --test dual_learning_contract`（9） |
| P2 W06 检查点/暂停/恢复/分裂 | ✅ | worker 状态制品为非执行 JSON（参数+Adam 矩+CPU RNG，原子写）；宿主校验 schema 与 run 谱系（跨 run 状态拼接拒绝），resume 落新 run 且账本继承，ControlledReplay 需声明确定性宿主，Paused 仅在保存成功后标记，fork 不触父；决定性契约：第 300 步杀进程、新进程续训至 600 步，参数与不中断运行逐字节一致 | `cargo test -p grove --test checkpoint_contract`（10） |
| P2 W07 历史重评与发布资格 | ✅ | 版本化评价协议；重评只追加不改写，跨协议混排拒绝（宽松协议通过不能发布到严格协议）；超时/崩溃以 0 分留在分母；硬门槛先于发布（均值再高也买不回崩溃的 repeat）；发布是带 expected-version 的指针切换；质量/成本非支配选择；独立验收预算共享、fork 不可翻倍 | `cargo test -p grove --test evaluation_contract`（11） |
| P2 W08 grove CLI 端到端 | ✅ | `grove` 二进制（新 grove-app crate）：demo/inspect/checkpoint/fork/resume/compare/select/**approve/decline** 全部映射库合同；协议为持久化记录，CLI 加载而非重建（flag 丢门槛无法发布——e2e 抓住的真漏洞）；`demo --case dual` 实测：基线 0.539 → 经真实暂停/续接的非线性候选 0.996（+45.7pp）。**G04 起 demo 不再自行发布**：只产出待审候选并打印确切的 `grove approve` 命令，e2e 随后人工批准；欠拟合候选被点名拒绝 | `cargo test -p grove-app`（6） |
| P3 W09 多 worker 群体协调 | ✅ | attempt 租约（过期/取消的 attempt 无法覆盖新进度）、提交携带 attempt id + head 期望版本、账单按消息 id 幂等、外部未知结果保持 unknown；分配政策为纯 Zio（基线+领先+多样性配额、共享账本、安全点暂停）；**并行是实测的**：两条 worker 进程窗口重叠 >500ms，A 在检查点被杀后 B 继续且 A 可续接，fork 共用一条账本 | `cargo test -p grove --test population_contract`（8）；`grove demo --case population --workers 2`（重叠 940ms，两支各 0.996） |
| P3 W10 产品 API 与授权发布 | ✅（历史任务范围） | 可选 HTTP、同源 JSON API、角色校验与 expected-version 发布；非 loopback 无令牌被拒。**G03 起 `serve` 会真正消费队列**：owner loop 领取 `Queued`、在独立进程执行、并持久化每个真实 Progress/Done/Failed（见 `app/src/runner.rs`）；`GET /api/events?run=&after_sequence=` 读持久日志；HTTP 操作回执落 `receipts` 表（重启后重放仍幂等，实测 `kill -9` 后同一 `operation_id` 返回同一 branch）；`resume` 从检查点**实际启动执行**（实测从 step 4 续接、父 run 不被覆盖）。**G04 起发布是 `POST /api/approve`**（令牌即认证，响应带审批 id，无 `approved: true` 字段）。**仍未闭合**：run 仍停在 `evaluating`——G04 交付了评价与人工批准能力，但 owner loop 尚无调用方自动走 `Evaluating → Accepted` | `cargo test -p grove-app --features http --test api_contract --test runner_contract`（14 + 历史 6）；`grove serve` |
| P3 W11 产品界面 | ✅（历史任务范围） | 同源 HTML、快照绑定预测、检查点谱系、模块合同与确认发布；实际推理走 torch worker。当前“已学习”字段实际上报告冻结数据集成员，不证明训练消费；事件不是持久 loss/progress 流；逻辑审查与升级仍 Planned | `cargo test -p grove-app --features http --test ui_contract`（历史 5）；原 UI 流程记录见交付计划 |
| P3 W12 撤回、保留与恢复 | ✅ | 从"必须存活"的根（活跃发布、分支 head、非终态 run、冻结视图）做可达性分析；保留期是**声明**的（store meta `retention.horizon_ms`），未声明即**一个都不删**；撤回信号沿冻结视图→run→检查点→快照传播并标记不可部署，发布与续接双双被拒而冻结成员不变；制品被删/被改字节返回 `artifact-unavailable`；重启回收过期租约并重取协调者所有权，旧 epoch 回执被拒 | `cargo test -p grove --test lifecycle_contract`（10） |
| P4 W13 模块组合 | ✅ | 模块声明消费/产出语义空间，组合按空间而非宽度校验（同宽不同义被拒）；共享参数组为一个演化单元；权限闭包取最大值（组合可要求更多不可更少）；组合产生新身份与多父谱系且父快照不变；联合计划强制整体评价。`demo --case modular` 分离演化两个模块、异构组合被拒并给出原因、合成体联合训练至 1.000 并以整体分数发布；非分类器模块报 `n/a` 而非伪造 0.0 | `cargo test -p grove --test composition_contract`（9）；`grove demo --case modular` |
| P4 W14 三索引经验 | ✅ | 结构/行为/语义三索引建立在**已有**信号与观察记录上，不另建事实库；行为指纹带探针集名，跨探针集不比较；语义向量带编码器与空间版本，版本不符且未显式迁移则拒绝。四项硬拒绝均有测试（语义近邻不并入不同 AST、探针集不混比、不越许可、被撤回来源的能力拒绝加载）；抽象晋升需原任务回归 + 新任务评价 | `cargo test -p grove --test memory_contract`（12）；`cargo test -p zio-cli --test lib_contract`（10）。**复用测量为负并如实记录**：未参与发现的任务上总描述长度 18→22，3 叶规模下抽象不划算 |
| P4 W15 扩展 recipe | ✅ | 软蒸馏、偏好排序、合法动作集内的演示克隆、自监督、延迟环境反馈、受限离散策略梯度；宿主强制声明（未声明信号、越预算、未知 recipe 均拒绝）。实测：软蒸馏 KL 0.6918→0.1264；偏好 0.5469→0.9531 且弃答不进目标；演示 0.9297，25% 非法动作被 mask 后 0.8932；自监督移动表征而任务指标单列 0.5117 仍不过门槛；延迟奖励 64/64 结算、未结算时参数变化恰为 0.0；策略梯度 −1.0000→0.7500 | `python -m unittest discover -s workers/torch/tests`（72）；`cargo test -p grove --test recipe_contract`（15）。**外部供应商未提供软输出，仅本地教师硬蒸馏已联调** |
| P4 W16 专家集成 | ✅ | 路由、专家版本、输出空间、组合规则与每调用预算共同构成一个快照身份；异构空间拒绝绑定，无可用专家 abstain，投票平局无胜者，`all-agree` 缺一专家即 abstain，单次调用按全部被调专家计费；群体一致只是带一致度的蒸馏目标，仍须走任务验收 | `cargo test -p grove --test ensemble_contract`（12）；`demo --case modular` 现场对比 vote 与 all-agree |
| P4 W17 全链路交付 | ✅ | `cargo test --workspace --all-features` 364 通过 / 0 失败；`python -m unittest discover -s workers/torch/tests` 72 通过；dual / population / modular 三个实际场景全部可运行 | 见交付计划第 12 节 |

P1 证明的是工程闭环（真实存储、真实信号生命周期、真实教师调用），
不含任何业务收益声明。P2 起需要真实张量训练、检查点恢复与多进程证据，
门槛见交付计划第 11 节。

**仍待验证或实现**：商业上游软输出与许可、真实业务门槛、GPU 推理成本，
以及 G1–G3 的生成执行、逻辑审查与升级闭环。历史组件证据不改变这些状态。

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

### Harness 与 Grove agent

旧 `zio-agent` 框架蓝图由 H00/G01/G02 收敛：公共 harness 持有模型与工具
会话、预算/取消，Zio agent 逻辑实际驱动步骤，Grove 持有学习和版本治理。
`lib/zio/agent.zio` 的 canned echo 已由真实 `agent-run` 循环替换：它决定重试
与停止，宿主通过 `harness-complete`/`harness-execute-zio` 提供模型调用与受限
执行（见 `app/src/agent.rs`、`learning/src/execution.rs`）。G02 已用 `omp acp` +
`minimax-code-cn/Minimax-m3` 完成 live 验收：首轮生成即产出真实计算的程序
（换输入可证），失败的首轮经诊断后在第二轮修订为 candidate；wire 上没有调用方
提供的 `system` 角色。
ACP 客户端/教师适配按 G06 验收，服务端按 G07 验收；不另起重叠框架。

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

下面保留旧功能 Phase 的依赖示意。当前实现主线的合同依赖以前面的工作包
表与统一实现计划为准，不能据此要求 Grove 等待完整 VM/JIT 或编辑器。

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
