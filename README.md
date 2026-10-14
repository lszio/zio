# Zio

> A General-Purpose Lisp.

Zio 是语义独立的通用 Lisp，Rust 承担引导实现、运行时机制与可信宿主，
而不是语言语义的定义。当前语言在 `langs/core/`、`langs/cli/`，Zio 库在
`libs/`，原生机制在 `contribs/native/`，站点在 `apps/`。
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
```

当前原生发行树与浏览器 WASM 分开构建。规划中的应用 bin 首轮是
bytecode + runtime 打包，不是机器码 AOT；应用 bin/WASM 不携带
LSP 等开发工具，动态能力按真实依赖保留。WASM 浏览器与 WASI 目标分别验收。
外部语言引擎尚不能宣称已全套捆绑。Groiz 是独立项目
[lszio/groiz](https://github.com/lszio/groiz)，不在本仓库，语言仓库不承载其业务与落地页。

[`docs/release.md`](docs/release.md) 记录现有发行方式、目标边界、
打标签、GitHub Actions 发布流水线与 crates.io 发布流程。
`push v*` 标签触发 `.github/workflows/release.yml`：先校验标签与 workspace
版本一致，再跑完整测试与自举契约，通过后
构建两个 Linux 平台目标（x86_64 与 aarch64）、发布六个 crate、发布两个 npm 包
[`@lszio/zio`](https://www.npmjs.com/package/@lszio/zio)（CLI：按版本下载
对应平台归档并运行）与
[`@lszio/zio-core`](https://www.npmjs.com/package/@lszio/zio-core)（WASM 引擎）、
推送 Docker 镜像 `ghcr.io/lszio/zio`（含 `zio` 可执行文件），
并附带校验和、WASM 产物与 `manifest.json` 组件清单创建 GitHub Release。
`NPM_TOKEN` 与 `CRATES_TOKEN` 需配置为仓库 secret。

`zio` 是语言统一入口：REPL、脚本运行、受限执行、`compile`、`image`、
`build`、`self-build`、`fmt` 格式化与 `lsp` 语言服务都在同一个二进制里；
`zio lsp` 启动与包内同版本的 `zio-lsp`，编辑器也可直接启动 `zio-lsp`。

发布面一览：

- **可执行文件**：Linux 双平台归档（x86_64／aarch64，`tools/install.sh` 装配，
  `bin/zio`、`bin/zio-lsp`、`bin/zio-env.sh`）、Docker 镜像
  `ghcr.io/lszio/zio`，以及 `zio build --target native` 的单文件应用。
  macOS 与 Windows 归档尚未交付：`zio-host` 的受限进程与文件系统机制目前是
  Linux-only 实现，缺少这两个平台的等价实现，归档不能假装已经支持。
  `ZIO_TARGET` 决定归档构建与安装的目标三元组；每个归档带
  `manifest.json`（版本、commit、组件校验和）。
- **Rust 执行库**：[`zio-core`](https://crates.io/crates/zio-core) crate——嵌入
  宿主执行或编译 Zio 代码（`wasm` feature 构建浏览器引擎）；`zio-host`
  通用传输/宿主机制、`zio-js` 原生 JS 引擎绑定、`zio-lsp` 语言服务、
  `zio-wasi` WASI 运行时与 `zio-cli` 语言入口随同版本发布。
- **npm 包**：`@lszio/zio`——CLI 包装，按版本下载匹配的平台归档并运行；
  `@lszio/zio-core`——同一引擎的 WASM 构建，浏览器或 Node 中
  定位并加载 Zio 运行时（`ZioSession`），与原生 CLI 行为一致。

## 落地页

`apps/site/` 是语言介绍、文档和 WASM playground，采用 Astro 静态构建。

```bash
cd apps/site
bun install
bun run dev        # 本地开发
bun run build      # 产出 apps/site/dist
```

- 主要入口：`/intro/`（语言介绍）、`/syntax/`（语法文档）、`/playground/`（可编辑多行源码、真实 WASM 执行）。
- `/reference/` 库参考在构建时扫描 `libs/**/*.zio` 的文件头与
  [`;; doc:` 注释](docs/libdoc-convention.md)自动生成——源码注释是唯一文档来源。
- 文档：`/docs/`、`/book/`、`/blog/` 从仓库 Markdown 构建。
- 引擎：`apps/site/public/wasm/` 是提交的 WASM 产物。改过 `langs/core/src/` 或
  `langs/core/assets/std/core.zio` 后先跑 `./tools/build-wasm.sh`，再构建站点，避免运行旧引擎。
- 路径前缀来自 `astro.config.mjs` 的 `base`；WASM 加载沿用 `src/lib/engine.ts` 的
  `BASE_URL` 拼接，避免相对资源路径随页面层级变化。
- 镜像：`Dockerfile` 是「bun 构建 → nginx 托管」两阶段，`docker-compose.dokploy.yml`
  用它部署。

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
| [docs/adrs.md](docs/adrs.md) | 架构决策记录（ADR-001 ~ ADR-018） |
| [docs/glossary.md](docs/glossary.md) | 术语参考 |
| [blog/INDEX.md](blog/INDEX.md) | 系列博文 |

## 设计定理

| # | 定理 | 状态 |
|---|------|------|
| 1 | **所有 mutable 状态必须显式** — 无 thread-local 全局变量 | ✅ 已达成 |
| 2 | **语言先行，库与应用用 Zio 实现** — Rust 承担引导运行时与必要通用原语 | 架构已确定 |
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
