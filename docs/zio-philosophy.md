# Zio 语言哲学

> Version 1.0 — 核心设计原则与语言哲学

---

## 1 本质定义

### 1.1 Zio 是什么？

Zio 是一门以同像性（homoiconicity）为基石的通用 Lisp 语言。代码即数据，数据即代码。宏在语法域变换程序，eval 在运行时域执行程序。两个域通过 Sexp ↔ Value 转换桥接。

在此基础上，Zio 构建了：

```text
Zio = 核心语言
    ├── Lisp 核心（Sexp + eval/apply + 宏）
    ├── ZOS（AMOP 运行时对象模型：Class · GF · Method · MOP · Package · Condition）
    └── Rust 宿主（零开销嵌入 · EvalEngine trait · NativeFn）
```

所有领域能力（Datalog、Agent、持久化集合等）都是通过宏 + MOP 构建的 `.zio` 扩展库，不进入核心。

### 1.2 核心命题

同像性是 Zio 的根。一切特性由此推导：

```text
同像性 → 宏系统 → 用户拥有与语言实现者相同的扩展能力
       → 代码可被程序读取、变换、生成
       → eval/apply 是元循环求值器
       → eval(env, expr) = agent(state, action)
```

Zio 的扩充分解：

```text
Zio = Lisp 核心（同像性 + eval/apply + 宏）
    + Rust 宿主（FFI + 嵌入 + 零开销）
    + ZOS（AMOP + MOP + 多分派）
    + 宏系统（模式匹配 → 显式重命名）
    + 扩展库（Datalog · Agent · 自学习）
    + 自举工具链（编辑器 · LSP · Debugger）
```

---

## 2 设计定理

### 定理 1: 所有 mutable 状态必须显式

**推论**: 无 thread-local 全局变量。所有可变状态由 `EvalContext` 持有，通过 `&dyn EvalEngine` trait 注入。

**状态**: 已达成 (0 thread_local! globals, 103 tests passing)。

**原理**: 隐藏的可变状态是测试、嵌入、并发的最大敌人。显式状态使系统可隔离、可 mock、可缩放。

### 定理 2: Rust 是合同边界，Lisp 是组合层

**推论**: 性能关键路径通过 `NativeFn` 用 Rust 实现。Lisp 层负责策略、组合、元编程。

**原理**: Rust 层提供最小、正确、经过测试的原语。Lisp 层通过宏、高阶函数、DSL 组合这些原语。两者通过 `EvalEngine` trait 解耦。

### 定理 3: 宏是用户扩展 eval 的方式

**推论**: 没有特殊形式不可用宏替代。任何新语言特性首选宏方案，特殊形式只作为最后手段。

**原理**: 同像性的核心价值在于「用户拥有跟语言实现者相同的扩展能力」。宏使 DSL 无需修改核心即可嵌入。

### 定理 4: 核心最小，其余是库

**推论**: 任何领域能力（Datalog、Agent、自学习、Clojure 风格集合）都不进入 Zio 核心。核心只包含：Sexp/Value/Eval、ZOS（Class/GF/MOP/Package/Condition）、Builtin（最小编程原语）。

**原理**: 核心的稳定性取决于它不做多少事。领域能力通过宏 + MOP + 库来构建，可以独立迭代、版本、替换。

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
ZOS 描述的是**运行时对象**，不是语言语法。Reader、Macro、Compiler 负责**生成**对象；ZOS 负责**运行**这些对象。

### 3.5 机制而非策略（Mechanism over Policy）
ZOS 只提供机制，不提供策略。Multiple Dispatch 是机制；Protocol 是策略。Immutable Entity、Datomic、AI Runtime 都是策略——全部通过宏 + MOP 构建。

---

## 4 核心特性

Zio 的核心特性构成一个自洽的整体，不是其他语言特性的组合：

| 特性 | 归属 | 说明 |
|------|------|------|
| **同像性** | 核心语言 | 代码即数据，宏在 Sexp 域变换 |
| **AMOP / MOP** | ZOS | 统一运行时对象模型 + 元对象协议 |
| **Generic Function / 多分派** | ZOS | 基于全部参数类型的行为分派 |
| **Condition / Restart** | ZOS | 带恢复选项的错误处理系统 |
| **Package** | ZOS | 符号命名空间管理 |
| **卫生宏（模式匹配）** | 核心语言 | 自动重命名的模式匹配宏 |
| **尾调用优化** | 核心语言 | 所有尾位置不建帧 |
| **持久化数据结构** | 核心语言 | 不可变 + 结构共享（im crate） |
| **零开销嵌入** | Rust 宿主 | 任意 Rust 程序可嵌入 Zio |

### 4.1 扩展库（纯 Zio 实现）
所有领域能力都是通过宏 + MOP 构建的 `.zio` 库，不进入核心：

| 库 | 目录 | 说明 |
|----|------|------|
| `zio-persistent` | `lib/zio/persistent.zio` | 持久化集合（Vector / Map / Set） |
| `zio-datalog` | `lib/zio/datalog.zio` | 内存 Datalog 数据库 |
| `zio-agent` | `lib/zio/agent/` | Agent 编排框架 |
| `zio-entity` | `lib/zio/entity.zio` | 带身份的对象 |
| `zio-protocol` | `lib/zio/protocol.zio` | Protocol 系统 |

### 4.2 自举
Zio 的自举路径是**工具链自举**，不是「编译器用 Zio 写」：

```
Phase 1-2: Rust 实现核心语言 + REPL
Phase 3-4: Zio 语言成熟 → 能写实质性程序
Phase 5+:  用 Zio 编写编辑器（语法高亮 + REPL 集成）
Phase 7+:  编辑器具备 LSP 能力（用 Zio 写）
```

当一个用 Zio 写的 Zio 编辑器成为主要开发界面时，语言就完成了工具链自举。
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

---

## 6 架构分层

```
┌──────────────────────────────────────────────────┐
│                   Application                      │
│   CLI · REPL · Editor · LSP · Embed · WASM        │
├──────────────────────────────────────────────────┤
│                 Extension Library                  │
│   zio-datalog · zio-agent · zio-ai · zio-persist  │
│   zio-entity · zio-protocol · zio-actor · zio-gr  │
├──────────────────────────────────────────────────┤
│               Standard Library (.zio)              │
│   collections · math · io · json · test · llm      │
├──────────────────────────────────────────────────┤
│              Runtime + Compiler (ZOS)              │
│   ZOS: Class · GF · Method · MOP · Package · Cond  │
│   Eval · TCO · Macroexpand · Compiler · JIT        │
├──────────────────────────────────────────────────┤
│                 Frontend (Reader)                   │
│   Reader · Parser · Sexp · Span · Reader Macro     │
├──────────────────────────────────────────────────┤
│               Rust 宿主层 (Host)                    │
│   EvalContext · EvalEngine · ModuleRegistry        │
│   NativeFn · IoHost · FFI · #[zio_export]          │
└──────────────────────────────────────────────────┘
```

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

### 7.3 扩展库

```lisp
;; Datalog 查询（库）
(require :zio.datalog)
(q '[:find ?name :where [?e :person/name ?name]] db)

;; Agent 编排（库）
(require :zio.agent)
(def agent (agent "assistant" "help user" [calculator]))
(agent/run agent "calculate 2^10")

;; 持久化集合（库）
(require :zio.persistent)
(def m (assoc {} :a 1 :b 2))
(get m :a)            ;; → 1
```

---

## 8 设计承诺

### 8.1 我们承诺

1. **向后兼容**：新版本不破坏已发布的 ZOS 协议和 `EvalEngine` trait
2. **正交独立**：无需加载 ZOS 即可使用核心 Lisp（嵌入场景）
3. **嵌入优先**：任何 Rust 程序都可以嵌入 Zio，无需异步运行时
4. **宏优先**：新语言特性首选宏方案，特殊形式为最后手段
5. **渐进用户**：从简单脚本到复杂系统编程，体验平滑
### 8.2 我们不承诺

1. 与其他 Lisp 方言的完全兼容（ZOS 是自己的对象模型，不是移植）
2. Java / JS / Python 生态兼容性（Zio 通过 Rust FFI 与 C ABI 对接）
3. 无 GC 性能担保（当前使用 Arc + im 结构共享，不引入追踪式 GC）
4. 通过 AOT 编译达到原生性能（Phase 6 JIT 是可选加速器，非硬依赖）
