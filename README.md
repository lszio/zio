# Zio

> A Modern Lisp for the Agent Era.
>
> **Zio** 是一门面向未来的通用 Lisp 语言，融合 Lisp 家族数十年的设计经验，并针对多平台、高性能、科学计算、人工智能以及 Agent 应用场景进行重新设计。

---

## 愿景

过去几十年中，Lisp 诞生了许多优秀实现：

* Common Lisp 提供了强大的对象系统与元对象协议
* Scheme 探索了极简语义与语言理论
* Racket 发展出了语言即平台的理念
* Clojure 展示了不可变数据与并发模型的力量
* Janet 证明了 Lisp 可以轻量且易嵌入
* Julia 将动态语言与高性能科学计算结合起来

Zio 希望继承这些优秀思想，并在此基础上构建一门适用于未来十年甚至更长时间的新一代 Lisp：

* 简洁而强大
* 动态而高性能
* 可扩展而稳定
* 支持 Agent 与 AI 原生开发
* 从嵌入式设备到云端集群统一运行

---

# 设计目标

## 通用编程语言

Zio 并非专用于某一领域。

它既适用于：

* 命令行工具
* Web 应用
* 桌面软件
* 服务端开发
* 游戏开发
* 自动化系统

也适用于：

* 科学计算
* 数据分析
* 机器学习
* AI Agent
* 知识图谱
* DSL 构建

---

## 多平台运行

一次编写，多端运行。

目标平台包括：

* Linux
* macOS
* Windows
* Android
* iOS
* WebAssembly
* Serverless
* Embedded Runtime

统一语言，统一生态。

---

## Agent First

Zio 从设计之初即考虑 Agent 场景。

程序不仅仅是文本。

代码、模块、能力、实体、事件都能够被运行时感知和查询。

未来 Agent 可以：

* 理解代码结构
* 修改代码
* 分析系统
* 自动构建 DSL
* 自动扩展应用能力

---

# 核心特性

## Homoiconic

代码即数据。

```lisp
(+ 1 2)
```

等价于：

```lisp
(list '+ 1 2)
```

程序可以直接操作程序自身。

---

## 强大的宏系统

Zio 提供多层级宏能力：

### Reader Macro

扩展语法读取器。

### Syntax Macro

基于语法树转换。

```lisp
(defmacro when ...)
```

### Meta Macro

编译阶段元编程。

```lisp
(deftransform ...)
```

开发者能够创造新的语言特性，而无需修改编译器。

---

## 元对象协议（MOP）

受到 Common Lisp 和 AMOP 启发。

对象系统本身可被扩展。

开发者可以：

* 自定义对象模型
* 自定义方法分派
* 自定义反射行为
* 构建领域专属运行时

---

## 多分派（Multiple Dispatch）

不仅对象决定行为。

所有参数共同参与方法选择。

```lisp
(collide ship asteroid)
```

比传统单分派面向对象系统更灵活。

---

## 协议系统（Protocol）

行为优先于继承。

```lisp
(defprotocol Drawable
  (draw self))
```

协议与类型解耦。

---

## 不可变优先

默认采用不可变数据结构。

```lisp
[]
{}
#{}
```

获得：

* 更简单的并发
* 更可靠的推理
* 更安全的程序结构

---

## 模式匹配

```lisp
(match value
  ...)
```

支持：

* 代数数据类型
* 枚举
* 结构解构
* 守卫条件

---

## 条件系统（Condition System）

超越传统异常机制。

支持：

* Error
* Warning
* Restart
* Recovery

程序出现问题时可以恢复执行，而不是直接终止。

---

## Actor 并发模型

```lisp
(spawn ...)
```

```lisp
(channel ...)
```

```lisp
(task ...)
```

结合：

* Actor
* Structured Concurrency

实现安全且可扩展的并发编程。

---

# 类型系统

Zio 采用渐进式类型设计。

开发者可以：

```lisp
(def x 1)
```

也可以：

```lisp
(def x:i64 1)
```

性能敏感部分可逐步增加类型信息。

无需在项目开始时承担完整静态类型负担。

---

# 科学计算

科学计算是 Zio 的重要目标之一。

计划提供：

* 多维数组
* 向量化运算
* BLAS/LAPACK 集成
* GPU 支持
* 自动微分
* 概率计算
* 数值优化

使 Zio 成为 AI 与科学计算的统一平台。

---

# AI 与知识计算

未来版本中，Zio 将支持：

* 知识图谱
* Entity 模型
* Event 模型
* Rule Engine
* Logic Query
* Agent Runtime

程序与知识统一表达。

---

# 模块系统

模块是语言的核心组织单元。

```lisp
(module math)

(module ai)

(module plc)
```

模块支持：

* 独立编译
* 热加载
* 沙箱执行
* 权限控制
* 能力声明

---

# 运行时架构

```text
Source
   │
   ▼
 Reader
   │
   ▼
 Parser
   │
   ▼
 Macro Expansion
   │
   ▼
 HIR
   │
   ▼
 MIR
   │
   ▼
 ZIR
```

统一中间表示。

随后可以生成：

```text
ZIR
 ├── Interpreter
 ├── JIT
 ├── Native
 └── WebAssembly
```

---

# 项目目标

第一阶段：

* Reader
* Parser
* Macro System
* Interpreter
* Module System
* Protocol
* Generic Function
* WebAssembly Runtime

第二阶段：

* 渐进式类型系统
* Native Compiler
* 高性能 GC
* 并发运行时

第三阶段：

* 科学计算生态
* GPU 支持
* Agent Runtime
* Knowledge Runtime
* 分布式执行

---

# 示例

```lisp
(module hello)

(defn main []
  (println "Hello, Zio!"))
```

---

# 非目标

Zio 不追求：

* 完全兼容 Common Lisp
* 完全兼容 Scheme
* 极简学术实验语言
* JVM 专属语言
* JavaScript 方言

Zio 希望成为一门独立发展的现代 Lisp。

---

# 哲学

> Programs must be simple enough for humans to understand,
> and rich enough for machines to evolve.

我们相信：

* 代码是知识
* 程序是模型
* 语言是构建世界的工具

欢迎加入 Zio。
