# Zio 系统架构

> Version 0.7 — Zio 核心/自举、Numa/Rill/Loom 官方独立库与 Grove 应用（2026-10-05）
>
> 本文件同时描述当前架构与未来目标。实现状态以
> [特性矩阵](feature-matrix.md)为唯一依据，生成的仓库数量见
> [项目状态](status.md)。实现细节见 [eval-pipeline.md](eval-pipeline.md)，
> ZOS 规范见 [zos-spec.md](zos-spec.md)，设计决策见 [adrs.md](adrs.md)。

---

## 1 概述

### 1.1 系统定义

```text
语言 Zio = Lisp 核心 + ZOS + 最小运行时/宿主 + 自举工具链
官方独立库 = Numa（计算） / Rill（CLI） / Loom（harness/ACP）
独立应用 Grove 使用语言与官方库；库和应用不是语言内置组成
```

详细哲学定义在 [zio-philosophy.md](zio-philosophy.md)。

### 1.2 目标用户

| 用户画像 | 场景 | Zio 价值 |
|----------|------|----------|
| 系统程序员 | CLI 工具、配置文件、脚本 | Rust 宿主与 Lisp 表达力；FFI/性能按实际能力验收，不宣称零开销 |
| LLM 开发者 | Agent 编排、tool-use | 可编码逻辑与执行证据；真实 harness 为规划 |
| Common Lisp 用户 | 现代 CL + Rust 生态 | CLOS + MOP + cargo |
| 教学 | 编程语言课程 | 代码最简、概念正交 |

### 1.3 设计定理

| # | 定理 | 状态 |
|---|------|------|
| 1 | **所有 mutable 状态必须显式** | ✅ 0 thread_local 全局变量 |
| 2 | **Rust 是合同边界，Lisp 是组合层** | ✅ NativeFn + EvalEngine |
| 3 | **宏是用户扩展 eval 的方式** | ✅ defmacro 可用 |
| 4 | **核心收敛，领域能力外置** | 保留 ZOS；基础设施与应用不反向进入核心；各组件状态见[特性矩阵](feature-matrix.md) |
| 5 | **协议比实现重要** | ZOS 子集为 [Experimental](feature-matrix.md)，完整 MOP 为规划 |

---

## 2 当前状态

### 2.1 可验证状态

当前 workspace 由 `zio-core`、`zio-cli`、`zio-ai`、`grove`、`grove-app`
五个 crate 组成。测试、特殊形式、native binding 和 runnable example 数量由
[`tools/project-status.sh`](../tools/project-status.sh) 生成，不在本页复制；
快照见[项目状态](status.md)。

### 2.2 组件成熟度

| 组件 | 状态 | 说明 |
|------|------|------|
| Reader 与语法展开 | Stable | reader unit tests |
| AST evaluator、闭包、核心宏与 stdlib | Stable | core tests 与 runnable examples |
| ZOS class 与 generic dispatch 子集 | Experimental | 受 `zos-concept.zio` 合同覆盖，API 仍可能变化；GF 可调用性经 `zos::apply` 协议（ADR-013） |
| Persistent library | Experimental | 全部函数可运行；map 迭代顺序未定义 |
| Datalog 存储（create-db/transact） | Experimental | `lib/zio/datalog.zio`；查询求值器为 stub（Planned） |
| Agent 框架 | Demo | 数据模型可运行；无 LLM 调用与工具执行 |
| 协议系统（defprotocol/extend-type） | Experimental | 经 ZOS 泛型函数分派 |
| Entity 模型 | Experimental | id 为 buffer 渲染；唯一身份待核心原语 |
| 并发原语（future/chan） | Experimental — 同步占位 | 见 ADR-012：无线程，同步求值 |
| 文件 I/O（经 IoHost） | Experimental | `load`/`slurp`/`spit`/`file-exists?`；BufferIoHost 提供内存 FS |
| WASM 构建 + 落地页 REPL | Experimental | `core/src/wasm.rs` + `site/` |
| AI 宿主协议（`LlmHost`/`EmbedHost`，外部 attach） | Experimental | [`ai/src/lib.rs`](../ai/src/lib.rs)（ADR-016）；无宿主 → `capability-denied:`；Mock 录制/回放 + `http` feature |
| LLM 提议器库与代际学习循环 | Experimental | [`lib/zio/proposer.zio`](../lib/zio/proposer.zio) + [`lib/zio/learn.zio`](../lib/zio/learn.zio)；白名单/规范化去重/预算/错误隔离在循环内 |
| Grove 内部宿主、CPU 训练与产品界面 | Experimental | `learning/`、`workers/torch/`、`app/`；组件与 demo 可复用，不代表新的 agent 产品主线完成 |
| Grove 同像性 agent 逻辑、生成执行与可审查升级 | Planned | [整体设计](self-learning-architecture.md)与 ADR-019；服务训练队列、真实进度与冻结保护尚待贯通 |
| ZIR、bytecode VM、JIT 与应用能力 | Planned | 只有批准设计，不是当前运行时 |

完整证据和应用能力状态见[特性矩阵](feature-matrix.md)。

---

## 3 分层架构

### 3.1 层视图

```text
独立应用：Grove（模块、学习/实验、独立评价、人工批准、Web）
                         ↓ 按需消费公开接口
官方独立库：Numa（计算） / Rill（CLI） / Loom（harness/ACP）
                         ↓ 必要时通过语言公开扩展接口接入
语言：Zio 核心与 ZOS / 标准语义 / 自举工具链
                         ↓ 宿主能力
Rust：最小运行时、I/O、native、生命周期与可信隔离

zio-cli 是语言二进制，目标消费 Rill；不是 Rill 库本身
```

这是目标职责图，不代表库已交付。Numa/Rill/Loom 独立版本、按需安装，
核心不反向依赖它们；Rill 可用于不含 Zio 的 CLI，Numa/Loom 按需提供 Zio
适配。ZOS 保留核心，应用不引入领域特殊形式；热路径不要求对象化。

### 3.2 层职责

| 层 | 做什么 | Crate | 语言 |
|----|--------|-------|------|
| **Host** | EvalContext、生命周期、全局状态 | `zio-core` | Rust |
| **Reader** | tokenize + Sexp parse + reader macro | `zio-core` | Rust |
| **Runtime** | eval/apply、TCO、special forms、macroexpand | `zio-core` | Rust |
| **ZOS** | Experimental Class/GF/Method subset; broader MOP is planned | `zio-core` | Rust |
| **Stdlib** | 当前 core macros/stdlib；更广标准库为规划 | `core` | Rust + Zio |
| **Extension Libs** | persistent（部分）、Datalog 存储、agent demo；查询求值与真实编排为 Planned | `lib/zio/*.zio` | **Zio** |
| **Grove application** | 独立应用；现有宿主、CPU 后端、CLI/HTTP/HTML，目标为同像性逻辑演化 | `grove` + `grove-app` | Rust + Zio + 外部后端 |

### 3.3 Crate 依赖图

**当前（五个 workspace crate）**：

```
zio-cli (CLI + REPL + 脚本执行 + --llm-replay 装配)
├── zio-core (reader + AST evaluator + experimental ZOS subset
│             + builtins/ 按域内建模块 + wasm 入口)
└── zio-ai (宿主 AI 能力协议：LlmHost/EmbedHost + Mock record/replay
            + http feature；依赖 zio-core，core 不依赖它)

grove-app (独立应用；当前 demo、CLI + 可选 HTTP/HTML)
├── grove (learning/：合同、制品、事务、协调、检查点、模块与经验)
│   ├── zio-core
│   └── zio-ai (可选 teacher-http 依赖；不表示产品教师已接通)
└── zio-core

workers/torch/ (应用经受约束进程协议调用的 CPU 参考后端；不是 crate)

lib/zio/*.zio (扩展库，Zio 源码实现；不是 workspace crate)
```

### 3.4 已确认的领域边界

| 组件 | 负责 | 不负责 |
|---|---|---|
| Zio + ZOS | 语言与对象语义、代码作为数据、执行与扩展入口 | 训练、产品治理、CLI 业务、ACP 会话 |
| Numa 官方计算库（Planned） | 连续数组、dtype/shape、数值/向量/矩阵与模型后端接口 | Agent 决策、教师治理、产品评价/发布 |
| Rill 官方 CLI 库（Planned） | 命令/子命令、参数、帮助、组合、终端 I/O、退出状态 | 求值器与 Grove 业务；`zio-cli` 是消费者，不是该库 |
| Loom 官方 harness 库（Planned） | 模型/工具会话、预算/取消、供应商、受约束执行接口、双向 ACP | Grove 学习目标、独立评价或批准；当前 `zio-ai` 只是迁移起点 |
| Grove 独立应用 | 模块组织、实验/反馈、运行、检查点、审查/发布、站点 | 新解释器、新数值引擎、必须先完成的通用学习框架 |

三个官方库按实际消费完善公开合同，不依赖 Grove 私有实现；独立维护版本、
安装资源和依赖清单，不预设微服务或插件平台。当前 `zio-ai` 与 `zio-cli`
仍是现有包名；目标 `loom/`、`rill/`、`numa/` 的迁移/创建在 H00/I00/C01 执行，
不在本次文档修订中改包。注册表包 ID 尚未核验，不能把短名当作已发布包。
Grove 先交付“真实 agent → 生成执行 → 证据 → 反馈候选 → 独立评价 → 人工批准”。
详情以[整体设计](self-learning-architecture.md)和 ADR-019 为准。

### 3.5 工具链自举（Planned）

1. 固定必需语言、数值、模块、源码位置与首轮 ZOS 语义合同。
2. 用 Zio 实现完整展开与分析；Rust AST 执行器作为初始运行环境及语义对照。
   当前 `macroexpand` 只反复展开最外层，不能直接视为完整编译前端。
3. 用版本化便携字节码与 Rust 最小执行后端引导 Zio 编译器，不同时把优化 ZIR/JIT 作为前提。
4. Zio 编译器编译自身，再用产出的编译器重复构建；对照规范化产物与程序行为。

Rust 保留最小运行时、宿主能力与必要性能原语。ZOS 继续属于核心，完整 MOP
是演进目标，但未实现的全部功能不作为首轮自举前提。Zio 应用自托管是沿途
验证，不等同工具链自举；Grove 逻辑演化与自举分别验收，两条线互不阻塞。

当前实施顺序、文件落点及阶段证据集中在
[统一实现计划](superpowers/plans/2026-10-05-zio-grove-convergence.md)：
T00–T03 验收语言/自举，C00–C02 验收 Numa，I00 验收 Rill，H00/G06/G07
验收 Loom/ACP，G00–G08 验收 Grove。库独立消费与无库语言构建均需验证；
工作包完成前维持 Planned。

---

## 4 规划性能目标

以下数字是设计目标，不是当前基准结果。当前可复现的 AST 测量见
[AST evaluator baseline](../benchmarks/README.md)；ZIR、VM 和 JIT 状态为
[Planned](feature-matrix.md)。

本页不复制某台机器上的当前耗时。已批准设计中的 VM/JIT 性能数字仅是未来
验收目标；比较必须使用 AST baseline 文档规定的相同 workload、engine、
release build 与 machine metadata。

## 5 跨平台目标

| 目标 | Rust target | 状态 |
|------|-------------|------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | ✅ 开发主力 |
| macOS ARM | `aarch64-apple-darwin` | ✅ CI |
| macOS x86_64 | `x86_64-apple-darwin` | ⏳ 需 CI |
| Windows | `x86_64-pc-windows-msvc` | ⏳ 待测试 |
| WASM | `wasm32-unknown-unknown` | ✅ Experimental（落地页 REPL） |
| ARM Linux | `aarch64-unknown-linux-gnu` | Phase 7 |

---

## 6 相关文档

| 文档 | 内容 |
|------|------|
| [zio-philosophy.md](zio-philosophy.md) | 语言哲学、设计定理、设计原则 |
| [feature-matrix.md](feature-matrix.md) | 权威能力状态与证据 |
| [status.md](status.md) | 自动生成的仓库事实 |
| [eval-pipeline.md](eval-pipeline.md) | Eval 循环、TCO、宏系统、编译器管线 |
| [zos-spec.md](zos-spec.md) | ZOS 完整规范（Class/GF/Method/MOP/Condition） |
| [roadmap.md](roadmap.md) | Phase 路线图、交付标准、依赖分析、应用蓝图 |
| [adrs.md](adrs.md) | 架构决策记录（ADR-001 ~ ADR-019） |
| [self-learning-architecture.md](self-learning-architecture.md) | Grove 独立应用、同像性逻辑、生成执行与受控升级 |
| [glossary.md](glossary.md) | 术语参考 |
| [superpowers/plans/2026-10-05-zio-grove-convergence.md](superpowers/plans/2026-10-05-zio-grove-convergence.md) | 当前实施计划、依赖与实际验收；旧 Phase/W 编号保留为历史 |
