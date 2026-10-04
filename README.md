# Zio

> A Modern Lisp for the Agent Era.

Zio 是一门面向未来的通用 Lisp 语言，用 Rust 实现。当前 workspace
包含三个 crate：`zio-core`、`zio-cli` 和 `zio-ai`（宿主 AI 能力协议，[ADR-016](docs/adrs.md)）。实时生成的测试、语法和内置
绑定数量见 [项目状态](docs/status.md)；能力成熟度以
[特性矩阵](docs/feature-matrix.md)为准。

仓库数量不在 README 中手工维护；运行 `tools/project-status.sh` 可重新生成
[项目状态](docs/status.md)。

## 哲学

Zio 是一门以同像性（homoiconicity）为基石的 Lisp 语言。代码即数据，数据即代码。
宏系统让用户拥有与语言实现者相同的扩展能力。

```text
Zio = Lisp 核心（同像性 + eval/apply + 宏）
    + Rust 宿主（FFI + 嵌入 + 零开销）
    + 统一运行时对象模型（ZOS：AMOP + MOP）
    + 规划中的扩展库生态（Datalog · Agent · 自学习）
```

核心原则：**核心最小，其余是库**。扩展库位于 `lib/zio/`，能力成熟度
各异并以[特性矩阵](docs/feature-matrix.md)为准：persistent 集合与
Datalog 存储为 Experimental（部分函数待核心支持），Datalog 查询求值、
Agent 真实编排（LLM 调用 / 工具执行）与自学习模型框架仍为 Planned。
core 内置的并发原语（`future-call` / `chan`）当前是同步占位语义，
见 [ADR-012](docs/adrs.md)。

详细哲学：[docs/zio-philosophy.md](docs/zio-philosophy.md)

## 快速开始

```bash
cd zio
cargo run
```

```text
Zio REPL
Press Ctrl+D or type (exit) to quit
zio> (+ 1 2 3)
6
zio> (defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
#<function (n)>
zio> (fib 10)
55
zio> (defmacro unless [test body] (list 'if test nil body))
#<macro unless (test body)>
zio> (unless false 42)
42
zio> (load "program.zio")
zio> (require :my.module)
```

## grove 自学习产品

grove 是一个独立的 crate 组（`learning/` = 宿主库，`app/` = 产品壳），
**不改变上面的普通 Zio CLI**：`cargo run` 启动的 REPL 不依赖任何学习组件。

三种运行形态，各自的能力边界是显式的：

```bash
# 1. 仅语言（无张量后端）：普通 Zio CLI 照常工作，grove 相关测试打印 skip
cargo test --workspace

# 2. 完整产品（CPU torch 后端）
python3 -m venv .venv
.venv/bin/pip install -r workers/torch/requirements.txt
.venv/bin/pip install --index-url https://download.pytorch.org/whl/cpu torch
cargo build -p grove-app --bin grove --features http

# 3. 产品服务（令牌从环境变量读取，绝不从 argv 读取）
export GROVE_TOKEN_READER=… GROVE_TOKEN_ANNOTATOR=…
export GROVE_TOKEN_OPERATOR=… GROVE_TOKEN_PUBLISHER=…
./target/debug/grove serve --root /tmp/grove-data --bind 127.0.0.1:8787
#   → http://127.0.0.1:8787/ 打开产品界面
```

一个令牌对应**一个角色**：reader 不能标注、annotator 不能训练、operator
不能发布，localhost 也不例外。无令牌的非 loopback 绑定会被直接拒绝。

三个可运行的真实场景：

```bash
./target/debug/grove demo --case dual       --root /tmp/grove-dual      --device cpu
./target/debug/grove demo --case population --root /tmp/grove-pop --workers 2 --device cpu
./target/debug/grove demo --case modular    --root /tmp/grove-modular    --device cpu
```

- **dual**：线性基线 0.539 → 经真实暂停/续接的非线性候选 0.996（+45.7pp），
  发布的是**真实训练权重**而非占位快照；
- **population**：两个真实隔离 worker 进程并行训练（实测窗口重叠 966ms），
  共用一条预算账本，僵尸回执被拒；
- **modular**：两个模块分离演化 → 异构空间组合被拒 → 合成体联合训练至
  1.000 并以整体分数发布。

隔离要求 `unshare -Urn --pid --mount --fork` 可用；不可用时 worker **拒绝
运行**，不降级为无隔离执行。缺少 torch 后端时相关能力明确失败或跳过，
不会以 mock 或常量标签假冒训练。

## 示例

[`examples/manifest.tsv`](examples/manifest.tsv) 中列出的所有文件都可由
CLI 运行，并受可执行示例合同测试保护。`datalog-concept.zio` 只演示查询
数据是可读取的 Zio 值，并不执行 Datalog 查询；Datalog evaluator 仍是
[Planned](docs/feature-matrix.md)。当前不支持 `#{...}` set literal，限制及
状态也记录在[特性矩阵](docs/feature-matrix.md)。

## 文档

| 文档 | 说明 |
|------|------|
| [docs/status.md](docs/status.md) | 自动生成的 workspace、测试和语言表面数量 |
| [docs/feature-matrix.md](docs/feature-matrix.md) | **权威能力状态：Stable / Experimental / Planned** |
| [docs/zio-philosophy.md](docs/zio-philosophy.md) | 语言哲学、设计定理、设计原则、特性来源 |
| [docs/zos-spec.md](docs/zos-spec.md) | **ZOS 完整规范**（Object/Class/GF/Method/MOP/Condition） |
| [docs/zio-architecture.md](docs/zio-architecture.md) | 系统架构总览、分层、组件状态 |
| [docs/eval-pipeline.md](docs/eval-pipeline.md) | Eval 循环、TCO、宏、编译器管线 |
| [docs/roadmap.md](docs/roadmap.md) | 分 Phase 路线图、交付标准、依赖分析、应用蓝图 |
| [docs/synthesis-plan.md](docs/synthesis-plan.md) | 程序合成模块计划：学习型提议（LLM 主线，遗传/RL/NN 扩展位）× eval 裁判 × 语言化记忆（含论文与汇报规划） |
| [grove 整体设计](docs/self-learning-architecture.md) | 基于 Zio 的自学习库与产品：多源反馈、代码/权重联合学习、检查点、群体与模块演化（设计基线） |
| [grove 交付计划](docs/superpowers/plans/2026-10-02-self-learning.md) | P1–P4、W00–W17 工作包、文件落点、依赖、真实验收与风险（W00–W17 已执行并验证；证据与未验证项见第 12 节） |
| [docs/adrs.md](docs/adrs.md) | 架构决策记录（ADR-001 ~ ADR-018） |
| [docs/glossary.md](docs/glossary.md) | 术语参考 |
| [blog/INDEX.md](blog/INDEX.md) | 系列博文 |

## 设计定理

| # | 定理 | 状态 |
|---|------|------|
| 1 | **所有 mutable 状态必须显式** — 无 thread-local 全局变量 | ✅ 已达成 |
| 2 | **Rust 是合同边界，Lisp 是组合层** — 性能关键路径用 NativeFn | ✅ 已达成 |
| 3 | **宏是用户扩展 eval 的方式** — 没有特殊形式不可用宏替代 | ✅ 已达成 |
| 4 | **核心最小，其余是库** — 领域能力不进入 Core | ✅ ZOS 规范中 |
| 5 | **协议比实现重要** — EvalEngine/MOP 是扩展契约 | ✅ 规范中 |

## 路线图一览

| Phase | 时间 | 交付物 |
|-------|------|--------|
| **Phase 1** — 核心稳定化 | 1-2 周 | Span 集成, Reader 扩展, 全 TCO, macroexpand, 200+ tests |
| **Phase 2** — ZOS Phase 1 | 2-3 周 | Class/GF/Method/Package/Condition, `defclass` `defgeneric` `defmethod` |
| **Phase 3** — 卫生宏 + ZOS Phase 2 | 2-3 周 | `syntax-rules` 模式匹配宏, MOP, 多分派, 完整反射 |
| **Phase 4** — 标准库 + 系统编程 | 2-4 周 | FFI, File I/O, 懒序列, Future/Channel, 包管理器, 标准库 |
| **Phase 5** — ZOS Phase 3 + 生态（Planned） | 2-3 周 | 官方扩展库（persistent、entity、protocol） |
| **Phase 6** — 高级生态（Planned） | 3-4 周 | zio-datalog, zio-agent, zio-ai, Baseline JIT |
| **Phase 7** — 生产化 | 持续 | LSP, Debugger, WASM, Profiler, 自举编辑器 |

以上是规划目标，不代表已经交付。详见 [docs/roadmap.md](docs/roadmap.md)，
当前实现状态以[特性矩阵](docs/feature-matrix.md)为准。

## License

MIT
