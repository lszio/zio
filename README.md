# Zio

> 一门语义独立的通用 Lisp。

Zio 是一门以同像性（homoiconicity）为基础的通用 Lisp：程序结构也是数据，
函数组合计算，宏把重复的代码结构组织成新的表达方式。
Rust 承担引导实现、运行时机制与可信宿主，而不是语言身份的定义。
语法与语义以[语言参考](docs/language-reference.md)为入口，
可用能力与限制以[特性矩阵](docs/feature-matrix.md)为准。

核心原则：**核心最小，其余是库**。语言核心在 `langs/core/`，扩展库在
`libs/`，原生宿主机制在 `contribs/native/`。设计哲学见
[docs/zio-philosophy.md](docs/zio-philosophy.md)。

## 示例

```lisp
;; hello.zio
(println "Hello, Zio!")
(println (+ 1 2 3))
```

```bash
$ zio hello.zio
Hello, Zio!
6
$ zio                              # REPL
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
```

`examples/` 提供更多可运行示例；`examples/manifest.tsv` 列出的文件均可由 CLI 运行。

## 安装（v0.2.2 alpha）

v0.2.2 是 alpha 预览：GitHub Release 为 prerelease，npm 走 `alpha` 标签
（`latest` 标签仍是 0.2.1）。原生产物目前只提供 **Linux x86_64 / aarch64**
归档与 Docker 镜像；macOS 与 Windows 尚未交付。

**npm（推荐）**

```bash
npm install -g @lszio/zio@alpha
```

CLI 包装包 [`@lszio/zio`](https://www.npmjs.com/package/@lszio/zio) 自身不带二进制：
首次运行按版本下载对应平台归档，校验外层校验和后，运行其中的 `zio`。

```bash
zio program.zio   # 运行脚本
zio               # REPL
zio fmt file.zio  # 格式化（--write 写回，--check 检查）
zio lsp           # 语言服务（stdio）
```

**源码装配（独立发行树，不依赖 checkout）**

```bash
tools/install.sh "$HOME/.local/opt/zio"  # 在源码根目录运行；默认 release 构建
source "$HOME/.local/opt/zio/bin/zio-env.sh"  # PATH 与 ZIO_PATH
zio program.zio
```

调试构建用 `ZIO_PROFILE=debug`。装配后的树自带 stdlib、库、工具链源码、
grammar 与文档，`manifest.json` 记录各组件校验和。

**Docker**

```bash
printf '(+ 1 2 3)\n' | docker run --rm -i ghcr.io/lszio/zio:0.2.2 -
```

按精确版本标签 `0.2.2` 拉取；源码从 stdin 读取（`-`）。

## 语言与工具链

- **一个 CLI**：`zio` 是统一入口——REPL、脚本运行、`compile`、
  `image save / inspect / run`（运行时镜像）、`build`（应用导出）、`fmt`、`lsp`。
- **自举编译器**：第一阶段 Zio 自托管 bytecode 编译器已实现；
  Rust 引导与 VM 保留为可信宿主。`zio self-build` 编译自身前端。
- **运行时镜像与应用导出**（`image` / `build --target native`）：已实现，
  处于 Experimental 状态，见[特性矩阵](docs/feature-matrix.md)。
- **原生 JS 宿主**：CLI 内置 QuickJS 绑定（`zio-js`），可用
  `libs/interop/js.zio` 在 Zio 中调用 JS；浏览器 JS realm 与原生不同，
  执行面一致性逐项验收，以特性矩阵为准。
- **WASM**：同一核心编译为 WASM 的引擎是独立包
  [`@lszio/zio-core`](https://www.npmjs.com/package/@lszio/zio-core)，
  面向浏览器与 Node 嵌入，与原生是不同宿主面。不承诺 Rust-free、
  长期会话、完整隔离或包管理能力。

## 库（`libs/`）

| 目录 | 内容 |
|------|------|
| `libs/std/` | persistent 集合、Datalog 查询子集、entity、pipeline、protocol |
| `libs/numa/` | 数值、向量、tensor |
| `libs/rill/` | CLI 应用设施 |
| `libs/loom/` | 会话、agent、proposer、ACP 协议 |
| `libs/learning/` | 程序合成与记忆（learn / memory / runner） |
| `libs/interop/` | 宿主互操作绑定（如 JS） |

各库成熟度不同，状态一律以[特性矩阵](docs/feature-matrix.md)为准。

## 文档

| 文档 | 说明 |
|------|------|
| [语言参考](docs/language-reference.md) | 语法与语义入口 |
| [特性矩阵](docs/feature-matrix.md) | **能力状态权威：Stable / Experimental / Planned** |
| [语言哲学](docs/zio-philosophy.md) | 设计定理与原则 |
| [ZOS 规范](docs/zos-spec.md) | Object / Class / GF / Method / MOP / Condition |
| [系统架构](docs/zio-architecture.md) | 组件分层与状态 |
| [Eval 管线](docs/eval-pipeline.md) | Eval 循环、TCO、宏、编译器管线 |
| [设计决策](docs/language-design-decisions.md) | 关键设计决策记录 |
| [发行说明](docs/release.md) | 发行方式与边界 |

## 参与

本地环境、源码位置、库文档约定与检查入口见[贡献指南](apps/site/content/contribute/index.mdx)。
较大的语义与兼容性变更先在 [GitHub Issues](https://github.com/lszio/zio/issues) 讨论；
报告问题时附上最小程序、版本、平台与实际输出。

## License

MIT
