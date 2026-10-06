# Zio 语言哲学

> Version 1.1 — 保留 ZOS 的核心收敛与独立应用边界（2026-10-05）
>
> 原则、目标与当前能力分开：AST evaluator 为 Stable，ZOS 子集、程序
> 合成及 Grove CPU 组件为 Experimental；完整编译工具链、Loom/ACP
> 与 Grove 可审查 agent 演化主线为 Planned。状态与证据见
> [特性矩阵](feature-matrix.md)，定位以 [ADR-019](adrs.md#adr-019-grove-独立应用与同像性逻辑演化) 为准。

---

## 1 本质定义

### 1.1 Zio 是什么？

Zio 是一门以同像性（homoiconicity）为基石的通用 Lisp 语言。代码即数据，数据即代码。宏在语法域变换程序，eval 在运行时域执行程序。两个域通过 Sexp ↔ Value 转换桥接。

当前可验证的实现基础是：

```text
Zio = Stable Lisp 核心（Reader + Sexp + AST eval/apply + 宏 + stdlib）
    + Experimental ZOS 子集（Class + Generic Function dispatch）
    + Rust 宿主（EvalContext + NativeFn）
```

Numa（数值计算）、Rill（CLI 组合）、Loom（Agent harness）是官方独立库，
不是语言内置特性；按需安装、独立版本发布，Grove 是消费这些库的独立应用。
现有 `.zio` 库与 Rust 宿主是复用起点，不表示三库已经交付。具体状态见[特性矩阵](feature-matrix.md)。

### 1.2 核心命题

同像性是 Zio 的根。一切特性由此推导：

```text
同像性 → 宏系统 → 用户拥有与语言实现者相同的扩展能力
       → 代码可被程序读取、变换、生成
       → eval/apply 执行显式程序逻辑
       → 可编码 agent 逻辑需要另外装配模型、工具、权限与会话
```

Zio 语言与生态的规划分解（不是当前组件清单）：

```text
Zio 语言 = 当前 AST Lisp 核心 + 保留在核心的 Experimental ZOS 子集
         + Planned Zio 展开 / 分析 / 编译器与最小执行后端
         + Planned 后续 ZIR / JIT / 编辑器 / LSP / Debugger
官方独立库 = Numa（计算） / Rill（CLI 组合） / Loom（Agent harness）
独立应用 = Grove（消费语言与库，治理学习、评价、检查点与发布）
依赖方向 = 库与 Grove → 语言；语言核心不反向依赖库或 Grove
```

三库的正式短名不等于已注册包 ID。未来规范目录与逻辑命名空间分别为
`numa/`、`rill/`、`loom/` 与 `numa/*`、`rill/*`、`loom/*`，不是语言组件
`zio/compute` 或 `zio/command`；包注册 ID 未定，不能把命名空间占位当安装命令。
当前 `zio-ai`/`ai/` 是 Loom 的复用起点，`zio-cli`/`cli/` 是未来消费 Rill
的可执行宿主，本次不重命名 crate、目录、命令或已有 API。
迁移计划为 `ai/` → `loom/`，新建独立 `rill/` 库（不把 `cli/src/lib.rs`
当公共命令库），新建 `numa/` 与 `numa/zio/compute.zio`，并把当前
`lib/zio/vector.zio` 迁到 `numa/zio/vector.zio`。这些是后续
实施目标，不是当前文件；普通语言 CLI 不要求 Loom，ACP 服务由 Grove
向 Loom 注入执行回调并通过未来 `grove acp serve` 装配。
所有新增能力仍为 Planned；批准依赖与验收见[统一实现计划](superpowers/plans/2026-10-05-zio-grove-convergence.md)。

---

## 2 设计定理

### 定理 1: 所有 mutable 状态必须显式

**推论**: 无 thread-local 全局变量。所有可变状态由 `EvalContext` 持有，通过 `&dyn EvalEngine` trait 注入。

**状态**: 已达成。测试数量由[项目状态](status.md)生成，能力成熟度见
[特性矩阵](feature-matrix.md)。

**原理**: 隐藏的可变状态是测试、嵌入、并发的最大敌人。显式状态使系统可隔离、可 mock、可缩放。

### 定理 2: Rust 是合同边界，Lisp 是组合层

**推论**: 性能关键路径通过 `NativeFn` 用 Rust 实现。Lisp 层负责策略、组合、元编程。

**原理**: Rust 层提供最小、正确、经过测试的原语。Lisp 层通过宏、高阶函数、DSL 组合这些原语。两者通过 `EvalEngine` trait 解耦。

### 定理 3: 宏是用户扩展 eval 的方式

**推论**: 没有特殊形式不可用宏替代。任何新语言特性首选宏方案，特殊形式只作为最后手段。

**原理**: 同像性的核心价值在于「用户拥有跟语言实现者相同的扩展能力」。宏使 DSL 无需修改核心即可嵌入。

### 定理 4: 核心收敛，库与应用独立

**推论**：核心收敛职责，但不移除 ZOS。Numa 承担计算，Rill 承担命令行
应用组织，Loom 承担模型与工具会话；学习目标、独立评价和发布治理属于
Grove。三库独立演进，公共能力有真实复用需求才抽取，不先建设通用框架。

**原理**：稳定的语言语义与可独立演进的产品策略分开。Grove 可以在内部
采用库结构，但第一身份是独立应用，不是“通用学习库 + 可选产品壳”。

### 定理 5: 协议比实现重要

**推论**: `EvalEngine` trait 是所有扩展的契约。MOP（Meta Object Protocol）是 ZOS 的扩展契约。实现可以替换，协议保持稳定。

**原理**: 系统长期可演化的关键是接口的稳定性，而不是实现的性能。

---

## 3 设计原则

### 3.1 最小（Minimal）
核心保持尽可能小。ZOS 只提供运行时最基本的能力——Object、Class、Generic Function、Method、Package、Condition。任何领域能力都不进入 Core。

### 3.2 正交（Orthogonal）
各模块互相独立。Class 不依赖 Entity。Generic Function 不依赖 Database。MOP 不依赖 Graph。正交性确保每个概念可以独立理解和测试。

### 3.3 可扩展（Extensible）
任何高级能力通过以下途径构建：

```
Reader Macro → 语法级扩展
Macro        → 语义级扩展
MOP          → 运行时级扩展
Library      → 模块级扩展
```

四个扩展点形成**递进系统**：从最轻量的 Reader Macro 到最重量级的 MOP。

### 3.4 运行时优先（Runtime First）
ZOS 描述的是**运行时对象**，不是语言语法。当前 Reader、Macro 和 AST
evaluator 读取、变换并执行对象；未来 ZIR/bytecode compiler 与 JIT 仍为
[Planned](feature-matrix.md)。

### 3.5 机制而非策略（Mechanism over Policy）
ZOS 只提供机制，不提供策略。Multiple Dispatch 是机制；Protocol 是策略。Immutable Entity、Datomic、AI Runtime 都是策略——全部通过宏 + MOP 构建。

---

## 4 核心特性

Zio 的核心特性构成一个自洽的整体，不是其他语言特性的组合：

| 特性 | 归属 | 当前与目标边界 |
|------|------|----------------|
| **同像性** | 核心语言 | 代码可读取、变换与生成；不等于安全或神经模型透明 |
| **AMOP / MOP** | 核心 ZOS | 统一对象方向保留；完整 MOP 为 Planned |
| **Generic Function / 多分派** | 核心 ZOS | 当前子集 Experimental；更广语义按规范逐项验收 |
| **Condition / Restart、Package** | 核心 ZOS | 现有子集与完整规范分开，见特性矩阵 |
| **宏** | 核心语言 | 当前 `defmacro`；完整卫生宏为 Planned |
| **尾调用优化** | 核心语言 | 普通函数 trampoline 与 `loop/recur` 已实现 |
| **持久化数据结构** | 核心值与库 | 当前 `im` 集合；完整扩展集合库另行验收 |
| **Rust 嵌入** | 宿主 | 可嵌入；不作零分配或零开销保证 |

### 4.1 规划中的扩展库

扩展的归属按职责决定，不要求所有性能或协议代码都写成宏/MOP：

| 能力 | 起点与落点 | 状态 |
|------|------------|------|
| Numa：数值数组/矩阵/向量与后端接口 | 复用 `lib/zio/vector.zio`、`workers/torch/` 的计算边界；连续 typed array 与公共计算接口按需求扩展，不持训练或发布治理 | 当前组件 Experimental；Numa Planned |
| Rill：CLI 应用组织 | 参数、子命令、帮助、命令组合、终端 I/O 与退出状态；`zio-cli` 当前为脚本/REPL 可执行入口，未来消费 Rill；不承担语言求值 | Rill Planned |
| Loom：模型与工具 harness | 复用当前 `zio-ai`；模型、工具、会话、预算、取消、provider 与 ACP，不进入 eval 特殊形式，不持 Grove 学习目标/评价/发布 | Loom Planned |
| Grove 独立应用 | `learning/`、`app/` 和 Zio 策略；首条主线为真实 agent 逻辑与受控演化 | 当前组件 Experimental；新主线 Planned |
| Datalog / Entity / Protocol / 集合 | 保留已有数据表达与占位模块；领域执行不因能加载文件而成立 | 各项状态见特性矩阵 |

### 4.2 自举
目标是 **Zio 编写自己的展开器、分析器和编译器**，Rust 保留最小运行时、
宿主边界及必要性能原语。采用现有 AST evaluator 引导，明确首轮语言与
ZOS 语义集合，再通过最小后端运行编译器并编译自身。

```text
Rust AST 引导 → Zio 展开/分析/编译器 → 编译器产物运行并编译自身
             → 第二/第三代规范化产物与行为对照 → 工具链自举验收
```

Zio 编写编辑器、LSP 或 Grove agent 是应用自托管，不等同编译器自举。
Grove 产品主线与工具链分别验收、互不作为完成前提；执行顺序见
[统一实现计划](superpowers/plans/2026-10-05-zio-grove-convergence.md)。
---

## 5 什么是 ZOS

ZOS（Zio Object System）是 Zio 的统一运行时对象模型。它不是传统 OOP 中的「对象系统」，而是一个**运行时对象协议**——定义了对象如何存在、类型如何组织、行为如何分派、运行时如何扩展。

详细的 ZOS 规范在 [zos-spec.md](zos-spec.md)。

### ZOS 的职责

- 定义运行时对象模型（Object / Type / Class / Slot）
- 定义行为系统（Generic Function / Method / 多分派）
- 提供 Meta Object Protocol（MOP）
- 提供统一运行时反射
- 管理符号（Package）
- 提供错误恢复（Condition System）

### ZOS 不负责

- 数据持久化（→ 库: `zio-persistent`）
- Entity 模型（→ 库: `zio-entity`）
- Protocol 系统（→ 库: `zio-protocol`）
- Datalog 查询（→ 库: `zio-datalog`）
- Agent / AI Runtime（→ 库: `zio-agent`）
- Actor / 并发模型（→ 库: `zio-actor`）
- Graph / 图分析（→ 库: `zio-graph`）

这些是职责名称，不是全部已实现的库名。AI/harness 与独立 Grove 应用
可复用 ZOS，但会话、训练、持久化和发布规则不属于对象语义。

---

## 6 架构分层

```text
当前：zio-cli / zio-ai / grove-app
                  ↓
      Grove 内部宿主 + CPU 后端 / Zio 标准库与领域组件
                  ↓
      zio-core：AST eval/apply、宏、核心值、Experimental ZOS 子集

目标：独立 Grove 应用、其他 CLI / 嵌入 / 工具链入口
                  ↓
      计算 / CLI 组织 / harness（模型、工具、ACP）公共基础设施
                  ↓
      Zio 标准库与自举工具链
                  ↓
      Zio + ZOS 核心、最小运行时与可信宿主边界
```

职责不等于必须新增 crate；运行时依赖方向见[架构总览](zio-architecture.md)。

---

## 7 基础示例

### 7.1 核心语言

```lisp
;; 基本算术
(+ 1 2 3)             ;; → 6

;; 函数定义 + 尾递归
(defn countdown [n]
  (if (zero? n)
    "done"
    (countdown (dec n))))

;; 宏
(defmacro unless [test body]
  (list 'if test nil body))

(unless false 42)     ;; → 42
```

### 7.2 ZOS 对象

```lisp
;; 定义类
(defclass point ()
  ((x :initarg :x :accessor point-x)
   (y :initarg :y :accessor point-y)))

;; 通用函数
(defgeneric draw (shape))

(defmethod draw ((p point))
  (println "Point at" (point-x p) (point-y p)))

(draw (make-instance 'point :x 10 :y 20))
```

### 7.3 规划中的扩展库

`persistent.zio`、`datalog.zio` 等文件存在不代表规划 API 均可用；
查询数据、集合基础操作、AI 宿主、数值合成与完整领域执行必须分开。
本页不把规划 API 写成可运行示例，具体入口和状态见[特性矩阵](feature-matrix.md)。

---

## 8 设计承诺

### 8.1 我们承诺

1. **显式合同演进**：公开语言语义和宿主接口改变时迁移全部消费者，记录不兼容变更，不作尚未发布协议永久冻结承诺
2. **核心保留 ZOS**：普通 Lisp 程序不必使用对象 API；对象语义仍属于核心，不拆成可选领域插件
3. **嵌入优先**：任何 Rust 程序都可以嵌入 Zio，无需异步运行时
4. **宏优先**：新语言特性首选宏方案，特殊形式为最后手段
5. **渐进用户**：从简单脚本到复杂系统编程，体验平滑
### 8.2 我们不承诺

1. 与其他 Lisp 方言的完全兼容（ZOS 是自己的对象模型，不是移植）
2. Java / JS / Python 生态兼容性（Zio 通过 Rust FFI 与 C ABI 对接）
3. 无 GC 性能担保（当前使用 Arc + im 结构共享，不引入追踪式 GC）
4. 通过 AOT 编译达到原生性能（JIT/VM 仍为
   [Planned](feature-matrix.md)，不是当前加速器）
