# Zio

> A General-Purpose Lisp.

Zio 是语义独立的通用 Lisp，Rust 承担引导实现、运行时机制与可信宿主，
而不是语言语义的定义。当前语言在 `langs/core/`、`langs/cli/`，Zio 库在
`libs/`，原生机制在 `contribs/native/`，站点与独立应用在 `apps/`。
当前语法与语义以[语言参考](docs/language-reference.md)为入口，能力状态以
[特性矩阵](docs/feature-matrix.md)为准，四层目录合同见[系统架构](docs/zio-architecture.md)。

**目标（规划）**有两条独立主线：用 Zio 编写元循环求值器与自举编译器；
保存、恢复运行时镜像，并导出应用 bin 与 WASM。外部解释型与编译型语言
通过引擎库接入，`inline` 是外语代码入口，不等于一律直接 `eval`，
更不是任意 Rust 源码直接求值。完整开发发行包目标是一次配齐解释器、
编译器、LSP、语法资源、WASM 与文档；这不是当前 installer 的完成状态。
已有 bytecode VM、Zio 编译器和自举合同、LSP 与 tree-sitter 资源不等于两条
主线均已完成。详见[语言与工具链设计](docs/superpowers/specs/2026-10-11-zio-language-toolchain-design.md)
与[执行计划](docs/superpowers/plans/2026-10-11-zio-language-toolchain.md)。

L01 提供显式 Zio reader／元循环求值／宏展开入口；L02 **首轮工具链自举已验收**：
完整 Zio 前端在 fresh context 编译自身，Stage 1/2/3 规范制品逐字节相同。
运行 `cargo run -p zio-cli -- self-build --verify`，默认写入
`target/zio-self-build/stage-{1,2,3}.json`；可用 `--out-dir PATH` 指定目录。
验收包含编译后的 reader／expander／evaluator／compiler 与 sum=10、closure=15。
Rust 引导和 VM 机制保留，默认源码执行器不变；这不是镜像、独立应用导出或完整发行包。
入口和机制边界见[语言参考](docs/language-reference.md#编译前端与自举命令)。

仓库数量不在 README 中手工维护；运行 `tools/project-status.sh` 可重新生成
[项目状态](docs/status.md)。

## 哲学

Zio 是一门以同像性（homoiconicity）为基石的 Lisp 语言。代码即数据，数据即代码。
宏系统让用户拥有与语言实现者相同的扩展能力。

```text
Zio = Lisp 核心（同像性 + eval/apply + 宏）
    + Rust 宿主（FFI + 嵌入，能力按实际合同验收）
    + Experimental ZOS 子集（完整 AMOP / MOP 为规划）
    + Zio 扩展库（Datalog · Agent · 自学习，各自成熟度不同）
```

核心原则：**核心最小，其余是库**。Zio 扩展库位于 `libs/`，能力成熟度
各异并以[特性矩阵](docs/feature-matrix.md)为准：persistent 集合与
Datalog 合取查询子集为 Experimental，规则与聚合等扩展仍为 Planned。
已有 Zio agent 生成/执行与学习合同见特性矩阵，不等于 Groiz 业务整体迁移完成。
core 内置的并发原语（`future-call` / `chan`）当前是同步占位语义，
见 [ADR-012](docs/adrs.md)。

详细哲学：[docs/zio-philosophy.md](docs/zio-philosophy.md)

## 快速开始

```bash
cd zio
cargo run -p zio-cli
cargo run -p zio-cli -- examples/toolchain/semantics.zio
cargo run -p zio-cli -- examples/toolchain/meta-eval.zio
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

## 安装与发布

```bash
# 从源码构建当前原生发行树（不依赖 checkout）
tools/install.sh /tmp/zio          # 调试构建
ZIO_PROFILE=release tools/install.sh /opt/zio

source /tmp/zio/bin/zio-env.sh    # PATH 与 ZIO_PATH
/tmp/zio/bin/zio program.zio      # 语言
/tmp/zio/bin/groiz --help         # 产品
```

当前原生发行树与浏览器 WASM 分开构建；installer 同时包含 Groiz 启动器，
但 Groiz 是可选独立应用，不是语言完整发行目标的必要组成。规划中的应用
bin 首轮是 bytecode + runtime 打包，不是机器码 AOT；应用 bin/WASM 不携带
LSP 等开发工具，动态能力按真实依赖保留。WASM 浏览器与 WASI 目标分别验收。
外部语言引擎尚不能宣称已全套捆绑。

Groiz 源码在独立仓库 [lszio/groiz](https://github.com/lszio/groiz)，本仓库以
submodule 挂载于 `apps/grove`（克隆用 `--recurse-submodules`）；构建与测试经
`cargo test --manifest-path apps/grove/Cargo.toml --workspace`。

[`docs/release.md`](docs/release.md) 记录现有发行方式、目标边界、
打标签、GitHub Actions 发布流水线与 crates.io 发布流程。
`push v*` 标签触发 `.github/workflows/release.yml`：先校验标签与 workspace
版本一致，再跑完整测试（含 submodule 固定的 Groiz 合同）与自举契约，通过后
构建五个平台目标、发布四个 crate、发布 npm 包
[`@lszio/zio-core`](https://www.npmjs.com/package/@lszio/zio-core)（WASM 引擎）、
推送 Docker 镜像 `ghcr.io/lszio/zio`（含 `zio` 可执行文件），
并附带校验和与 WASM 产物创建 GitHub Release。`NPM_TOKEN` 需配置为仓库 secret。

发布面一览：

- **可执行文件**：五平台归档（`tools/install.sh` 装配，`bin/zio`、`bin/groiz`）、
  Docker 镜像 `ghcr.io/lszio/zio`，以及 `zio build --target native` 的单文件应用。
- **Rust 执行库**：[`zio-core`](https://crates.io/crates/zio-core) crate——嵌入
  宿主执行或编译 Zio 代码（`wasm` feature 构建浏览器引擎）；`zio-host`
  通用传输/宿主机制随附发布。
- **npm 包**：`@lszio/zio-core`——同一引擎的 WASM 构建，浏览器或 Node 中
  定位并加载 Zio 运行时（`ZioSession`），与原生 CLI 行为一致。

## 落地页

`apps/site/` 是语言介绍、文档和 WASM playground，采用 Astro 静态构建；站点中的 Groiz 介绍路由不等于独立 `apps/grove/` 产品。

```bash
cd apps/site
bun install
bun run dev        # 本地开发
bun run build      # 产出 apps/site/dist
```

- 主要入口：`/intro/`（语言介绍）、`/syntax/`（语法文档）、`/playground/`（可编辑多行源码、真实 WASM 执行）。
- `/reference/` 库参考在构建时扫描 `libs/**/*.zio` 的文件头与
  [`;; doc:` 注释](docs/libdoc-convention.md)自动生成——源码注释是唯一文档来源。
- 文档：`/docs/`、`/book/`、`/blog/` 从仓库 Markdown 构建；Groiz 介绍在独立的 `/grove/` 路由，应用源码在 `apps/grove/`。
- 引擎：`apps/site/public/wasm/` 是提交的 WASM 产物。改过 `langs/core/src/` 或
  `libs/std/core.zio` 后先跑 `./tools/build-wasm.sh`，再构建站点，避免运行旧引擎。
- 路径前缀来自 `astro.config.mjs` 的 `base`；WASM 加载沿用 `src/lib/engine.ts` 的
  `BASE_URL` 拼接，避免相对资源路径随页面层级变化。
- 镜像：`Dockerfile` 是「bun 构建 → nginx 托管」两阶段，`docker-compose.dokploy.yml`
  用它部署。

## grove 自学习产品

运行与预览环境见 [docs/deployment.md](docs/deployment.md)。

Groiz 是独立仓库 [lszio/groiz](https://github.com/lszio/groiz)，以 submodule
形式挂载在 `apps/grove/`。`apps/grove/main.zio` 是实际 Zio 入口，
由宿主按 keyword 授权预算后显式调用；CLI 分发走 `libs/rill/cli.zio`，
HTTP 路由在 `apps/grove/api.zio`。

```bash
# 用语言 CLI 直接运行（开发）
./target/debug/zio --app apps/grove/main.zio \
    --app-root apps/grove --app-share . \
    --app-root-dir /tmp/grove --args --help

# 用安装后的发行版（无 checkout 依赖）
tools/install.sh /tmp/zio
GROIZ_ROOT=/tmp/groiz /tmp/zio/bin/groiz --help
```

产品命令：`demo`（dual/population/modular）、`inspect`、`checkpoint`、`fork`、
`resume`、`compare`、`select`、`approve`、`decline`、`run`、`serve`。

能力边界由 `tools/install.sh` 生成的 `bin/groiz` 启动器显式授予，
并作为一张 keyword map 交给 Zio 入口；启动器不会从它即将运行的源码里
读取任何授权信息——否则候选程序指定自己的张量后端会让隔离形同虚设。

### 迁移状态

存储、审批、记录与 HTTP 路由已在 Zio 中实现（`apps/grove/{store,codec,
contracts,artifacts,feedback,api}.zio`），宿主能力经
`contribs/native/host` 通用机制提供。历史 Rust crate
`apps/grove/native/{learning,app}` 仍在，测试覆盖其既有契约，
待新路径完全替代后删除。

三种运行形态，各自的能力边界是显式的：

```bash
# 1. 仅语言（无张量后端）：普通 Zio CLI 照常工作，grove 相关测试打印 skip
cargo test --workspace

# 2. 完整产品（CPU torch 后端）
python3 -m venv .venv
.venv/bin/pip install -r apps/grove/workers/torch/requirements.txt
.venv/bin/pip install --index-url https://download.pytorch.org/whl/cpu torch
cargo build -p grove-app --bin grove --features http

# 3. 产品服务（令牌从环境变量读取，绝不从 argv 读取）
export GROIZ_TOKEN_READER=… GROIZ_TOKEN_ANNOTATOR=…
export GROIZ_TOKEN_OPERATOR=… GROIZ_TOKEN_PUBLISHER=…
./target/debug/grove serve --root /tmp/grove-data --bind 127.0.0.1:8787
#   → http://127.0.0.1:8787/ 打开产品界面
```

一个令牌对应**一个角色**：reader 不能标注、annotator 不能训练、operator
不能发布，localhost 也不例外。无令牌的非 loopback 绑定会被直接拒绝。

历史验收覆盖以下真实场景；这些数字不是本轮目录迁移重新实跑的结果。当前 demo 停在待批准候选，按输出的 `grove approve` 命令人工批准后才能发布：

```bash
./target/debug/grove demo --case dual       --root /tmp/grove-dual      --device cpu
./target/debug/grove demo --case population --root /tmp/grove-pop --workers 2 --device cpu
./target/debug/grove demo --case modular    --root /tmp/grove-modular    --device cpu
```

- **dual**：线性基线 0.539 → 经真实暂停/续接的非线性候选 0.996（+45.7pp），
  批准后发布的是**真实训练权重**而非占位快照；
- **population**：两个真实隔离 worker 进程并行训练（实测窗口重叠 966ms），
  共用一条预算账本，僵尸回执被拒；
- **modular**：两个模块分离演化 → 异构空间组合被拒 → 合成体联合训练至
  1.000 并以整体分数获得待批准资格。

隔离要求 `unshare -Urn --pid --mount --fork` 可用；不可用时 worker **拒绝
运行**，不降级为无隔离执行。缺少 torch 后端时相关能力明确失败或跳过，
不会以 mock 或常量标签假冒训练。

## 示例

[`examples/manifest.tsv`](examples/manifest.tsv) 中列出的所有文件都可由
CLI 运行，并受可执行示例合同测试保护。`datalog-concept.zio` 只演示查询
数据是可读取的 Zio 值，并不执行 Datalog 查询；Datalog evaluator 仍是
[Planned](docs/feature-matrix.md)。`#{...}` set literal 现已可用（map-backed
`set`，由 bootstrap 提供），quasiquote（`` ` `` / `~` / `~@`）为
Experimental，状态见[特性矩阵](docs/feature-matrix.md)。

## 文档

| 文档 | 说明 |
|------|------|
| [docs/status.md](docs/status.md) | 自动生成的 workspace、测试和语言表面数量 |
| [docs/feature-matrix.md](docs/feature-matrix.md) | **权威能力状态：Stable / Experimental / Planned** |
| [docs/zio-philosophy.md](docs/zio-philosophy.md) | 语言哲学、设计定理、设计原则、特性来源 |
| [docs/zos-spec.md](docs/zos-spec.md) | **ZOS 完整规范**（Object/Class/GF/Method/MOP/Condition） |
| [docs/zio-architecture.md](docs/zio-architecture.md) | 系统架构总览、分层、组件状态 |
| [语言与工具链设计](docs/superpowers/specs/2026-10-11-zio-language-toolchain-design.md) | 通用 Lisp、自举、运行时镜像、外部引擎与完整发行目标（规划） |
| [语言与工具链计划](docs/superpowers/plans/2026-10-11-zio-language-toolchain.md) | 分阶段实施与验收；不是本轮已交付能力 |
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
| 2 | **语言先行，库与应用用 Zio 实现** — Rust 承担引导运行时与必要通用原语 | 架构已确定；现有 Groiz Rust 业务尚待迁移 |
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
