# Zio 语言、镜像与完整工具链设计

日期：2026-10-11。状态：方向已由用户确认；本文规定后续实现合同，不声明这些目标已经交付。

## 1. 定位与范围

Zio 是语义独立的通用 Lisp：以统一 S-expression、代码即数据、函数与宏组合程序；既能解释执行，也能编译。Rust 是当前引导实现与必要机制层，不是语言身份，也不是领域库与应用业务的默认实现语言。

同时建设两条主线，不能互相替代：

1. **自描述与自举**：用 Zio 编写 reader、求值器、展开器、分析器和编译器；元循环求值器能执行约定语义，编译后的编译器能再次编译自身。
2. **运行时镜像与导出**：保存已加载的代码、模块与可恢复状态；恢复交互环境，或从源码／镜像导出独立原生应用和 WASM 应用。

外部解释型和编译型语言通过显式导入的引擎库接入。一次安装获得完整 Zio 开发发行版；应用产物只携带实际依赖。Grove、计算、AI 与其他领域应用不参与定义核心语义。

不把完整 Node/npm 兼容、任意外语运行时内存快照、机器码 AOT、JIT、全平台立即交付作为前述目标的同义词。这些边界不是删除 JS/Rust、自举、镜像或 bin/WASM 交付范围。

## 2. 本轮实现事实与证据边界

| 现有基础 | 可检查来源 | 不能据此宣称 |
| --- | --- | --- |
| Rust reader、AST evaluator、宏与 ZOS 子集 | `langs/core/src/{reader,eval.rs,macros.rs,zos}` | 已有 Zio 元循环求值器或完整 MOP |
| Zio 编译器与 Rust 指令 VM | `langs/compiler/compiler.zio`、`langs/core/src/bytecode.rs` | 编译链所有语义均已迁到 Zio，或已有机器码／WASM 直接代码生成 |
| 多代编译与语义对照检查 | `langs/core/tests/self_hosting.rs`、`bytecode::self_build` | 本轮已实跑固定点验收，或编译器能独立于全部 Rust 前端运行 |
| 浏览器 JS bridge | `langs/core/src/wasm.rs` | 原生 CLI 有嵌入 JS 引擎、Node/npm 兼容或安全沙箱 |
| JS 对象句柄及 dotted resolver | 同上；`resolve_js_symbol` 的注释限定解释器路径 | 已有解释／编译一致的通用引擎接口 |
| LSP、Tree-sitter | `contribs/language-server/`、`contribs/tree-sitter-zio/` | 当前完整工具链发行验收已通过 |
| 发行装配 | `tools/install.sh`、`tools/build-wasm.sh`、`.github/workflows/release.yml` | 当前原生包已包含全部 WASM、语法、文档与外部引擎 |
| 模块产物序列化 | `bytecode::{serialize_module,deserialize_module}` | 能保存运行时闭包环境、循环对象图或任意宿主对象 |

本轮讨论中当前源码 CLI 实跑：threading 表达式返回 20、quasiquote 返回 `(+ 3 1)`、编译入口生成两个函数记录；原生 `js/Math.max` 报未绑定。此证据仅覆盖这些表达式。新的实现状态应按工作包的真实命令更新[特性矩阵](../../feature-matrix.md)。

## 3. 稳定边界与目录

沿用既有四层，不再搬目录或新建万能平台：

| 位置 | 职责 |
| --- | --- |
| `langs/` | 引导运行时、Zio reader／求值器／展开分析编译器、统一语言 CLI |
| `libs/` | 标准库、领域库、外部语言库的 Zio 接口与宏 |
| `contribs/` | 原生宿主、外部引擎适配器、语言服务、Tree-sitter、编辑器资源 |
| `apps/` | 语言站点、Grove 及其他独立应用 |

现有 `EvalEngine`、`NativeFn`、`Value` 是实现入口，不是永久语言规范。语义由语言参考与行为合同规定，未来实现可以更换。Rust 机制层承担内存、指令执行、I/O、可信隔离、引擎／ABI 适配及有实测需求的原语；求值策略、语法展开、分析与代码生成逐步迁到 Zio。

宏组合已有机制，不能凭空提供绑定、宿主执行或资源权限。导入库增加能力，不隐式改变 reader。外语块内容不具备 Zio 同像性，Zio 宏处理的是有来源位置的源码块及其输入输出合同。

## 4. 统一语义与自举

### 4.1 语义入口

源码走同一 reader、展开与分析合同；解释器和编译器共享规范，不分别定义作用域、求值次序、truthiness、整数边界、集合类型、异常、模块或外部调用。

保持当前 S-expression、`()` 调用／列表、`[]` 向量与绑定、`{}` 映射、`#{}` 集合、quote／quasiquote 基础。不趁工具链工作重新设计整门语法。规范统一 `let`／`let*`、`cond` 子句、参数／rest、模块路径、错误位置及多行源码表示；任何不兼容变更必须迁移所有实例、reader、Tree-sitter、LSP、站点及编译器消费者。

### 4.2 工具链代次

- Stage 0：当前 Rust reader／AST 与 VM 引导 Zio 工具链，是初始运行环境和对照实现。
- Stage 1：Zio 工具链生成自身的编译模块。
- Stage 2：执行 Stage 1 编译器，再编译同一份工具链源码。
- Stage 3：执行 Stage 2 重复构建；规范化模块字节相等，且同一语义语料在 Rust 对照、Zio 求值器、编译执行中值／错误／可观察副作用一致。

元循环验收与固定点验收分别报告。固定点比较实际规范化字节，不只依赖哈希；排除仅构建路径／时间戳等明确非语义元数据，不能忽略指令或常量差异。

最终首轮语义集合覆盖仓库已公开承诺的运行能力，包括模块、过程宏、`syntax-rules` 已有子集、闭包、尾位置、异常与 ZOS 已有子集；不把同步队列占位升级为真正并发，也不要求全部规划 MOP。内部先跑纯语言子集，但不能将其称为完整元循环交付。

## 5. 四类产物

| 类别 | 合同 | 默认依赖 |
| --- | --- | --- |
| 完整开发发行版 | REPL、解释器、VM、编译器、语言服务、语法查询、标准库、工具链源码、WASM、文档／示例、发行清单同版本 | 完整开发组件；可选引擎包明确列出 |
| 编译模块 `.zioc` | 版本化代码、常量、导入导出、调试来源和能力需求；不包含交互状态 | 兼容 Zio 运行时与声明依赖 |
| 运行时镜像 `.zio-image` | 根集合、对象图、环境、模块、函数／宏／类状态、来源与资源描述；不是进程内存转储 | 格式／运行时兼容及恢复宿主授权 |
| 应用产物 | 入口＋编译模块或初始镜像＋必要运行时与引擎／资源 | 运行无需 checkout、Zio 开发安装或联网下载 |

后缀和下面命令是规划接口，不是现有 CLI。编译模块复用现有版本化 wire 表示，不另造平行字节码。

### 5.1 原生与 WASM

首轮 native 应用采用 **字节码／镜像 + 原生 Zio 运行时**，不是机器码 AOT。依赖操作系统标准运行环境可以声明，但不能偷偷依赖 rustc、源码树或外部 `zio`。

WASM 有两个明确 profile：

- `wasm-browser`：WASM 模块＋JS 加载胶水＋所需资源；JS 使用浏览器引擎，DOM 只能在此 profile 显式提供。
- `wasm-wasi`：WASI 模块及声明的 WASI imports；不能包含 DOM／浏览器全局依赖。JS 需求必须带可用引擎，否则构建时报目标不支持，不能在运行中静默忽略。

两个 profile 都在计划范围内，单个 profile 验收不代表另一个已交付。直接机器码／WASM codegen 与 JIT 不作为首轮打包前提。

### 5.2 动态能力与裁剪

静态依赖可裁剪；若程序使用动态 `eval`、宏展开、动态模块或引擎选择，就保留该能力所需组件，或要求显式封闭依赖集合并验证。不能裁掉 reader 后仍宣称 `eval` 完整可用，也不能隐式关闭动态能力以缩小产物。应用默认不带 LSP、编辑器语法和文档。

## 6. 镜像合同

### 6.1 保存时刻与根

仅在求值／编译已退出、没有在途宿主调用的静止边界保存。首轮不保存原生线程栈或指令中途续体；正在运行的任务／调用明确拒绝。根包括选择的模块／会话绑定、类与泛型方法、入口及可选交互状态；不是扫描并保存整个宿主进程。

### 6.2 对象图

格式用对象 ID＋引用边表示用户可观察身份、共享与循环。保存闭包捕获、环境父链、可变绑定、宏环境、模块导出、类继承／slot／方法与 buffer 内容。内部不可观察的 persistent 数据结构布局不必原样保存。实现先建节点，再补引用，不通过递归复制破坏共享或遇循环无限递归。

当前 `Arc<Env>`、`Value::Object` trait object、Rust 闭包不能直接 serde。已有实例 clone 语义也不能被保存器悄悄改变；先固定运行时身份／可变单元合同，再建立编码。

### 6.3 原生函数与外部资源

原生函数通过版本化 binding ID 重新安装；不能把函数指针、地址、Rust closure 捕获或当前 NativeFn 计数 ID 写入镜像。非注册宿主闭包拒绝并报告根路径。

文件、socket、HTTP server、channel 等外部资源默认不可保存。只允许宿主注册的可信恢复器读取资源描述并在新授权下创建资源。结束的 future 可以记录完成值；未完成 future／channel 状态没有明确 portable 合同时拒绝。JS/Rust opaque 对象无 codec 时拒绝，不默默变成字符串、nil 或 JSON。必要数据先显式转换为纯 Zio 值；引擎内状态 codec 后续逐项支持，不承诺任意引擎 heap 快照。

### 6.4 恢复、兼容与安全

加载前有界校验格式版本、对象数、边数、字节数、引用有效性、模块与 binding 版本和 target profile；校验期间不执行用户代码。构造到新的 context，全部成功后才交给用户，失败不污染现有会话。禁止 pickle 式任意反序列化执行。

镜像记录所需能力，不保存授权实例、密钥、令牌、绝对宿主地址或活动连接。恢复器能力不能超过宿主本轮授予的能力。完整恢复阶段可以按明确授权执行模块初始化／资源重建，不将整个镜像加载视为安全读取；不可信镜像应在隔离执行环境运行。

同 runtime/格式版本的 portable 子集要求跨 native/browser/WASI 恢复；含平台资源的镜像仅在匹配 profile 与恢复器下恢复，否则失败。不得以操作系统内存快照替代此合同。

## 7. 外部语言引擎与 inline

### 7.1 最小引擎能力

只在真实 JS／Rust 接入所需处定义接口：源码执行、源码编译、模块加载、函数／对象调用和资源释放。每个引擎声明支持能力、目标、版本与权限需求；缺能力时明确拒绝，不自动 fallback。

解释与编译 Zio 都调用同一组显式绑定。替换当前仅解释器可查找的 JS dotted resolver 依赖，不为编译器另造隐式 hook。便利名称必须可在编译时绑定或经相同已安装库的解析函数调用。

纯值转换规定 nil/null/undefined、i64/JS Number/BigInt、非有限数、关键字／字符串键、集合、字节序列；不得静默损失大整数精度或把 keyword/string 冲突覆盖。对象保留所属引擎实例与身份，跨实例引用拒绝；回调、异常和 Promise 采用显式接口，不能将 Promise 当同步完成值。

### 7.2 JS 与 Rust 路径

JS 首个原生实现采用嵌入式 QuickJS 系列引擎；版本、绑定、许可证、支持目标与中断能力由 J00 的可运行选型验收锁定，而非先加一个未经验证的依赖。浏览器复用宿主 JS。两者保证规定的 ECMAScript／传值子集，不宣称 Node/npm 或 DOM 跨宿主兼容。

Rust 路径为构建时调用固定工具链、编译版本化 ABI 模块、加载并调用导出；不是任意 Rust `eval`，不暴露 Rust 不稳定内存布局。需要运行时编译时，产物必须声明并携带／定位指定编译器，不能从不可信源码或环境 PATH 选择可执行文件。Rust 是可信原生执行，ABI 与超时不构成沙箱；不可信源码走受隔离 worker。

### 7.3 源码块与阶段

规划的标准宏入口：`(inline engine options source)`，由 `libs/interop/inline.zio` 提供；`engine` 是已导入引擎库暴露的描述值，不是全局 reader 模式开关。

options 必须指定 `:phase`（`:run` 或 `:build`）、`:mode`（`:expression`、`:function` 或 `:module`）和需要时的 `:bindings`。引擎不支持组合时拒绝。原始源码编译策略是引擎合同，不是 reader 推断。

```lisp
;; 规划接口，不可直接复制到当前 REPL
(require :libs.interop.js :as js)
(require :libs.interop.inline :refer [inline])
(inline js/engine {:phase :run :mode :expression} "Math.max(1, 2)")
(inline js/engine {:phase :run :mode :function :bindings {:x 7}}
  "(x) => x * x")
```

`:bindings` 的值在调用时求值一次，按名称传入；不替换源码字符。构建阶段仅允许明确常量或显式构建输入，不能访问调用者运行时局部变量。外语 `${...}` 保持外语语义。`:module` 返回模块／导出句柄，`:function` 执行显式函数边界，`:expression` 返回表达式值；各引擎公布支持项。

新增通用原始文本格式采用带标签 fenced 字符串：`#"TAG` 起始直到单独一行 `TAG"#`，内容不进行 Zio 转义或插值；结束标记冲突可改 TAG。TAG 为 ASCII 字母开头、后续字母数字下划线，起始行之后换行是结构分隔，内容自身换行保留。未闭合报起始 source span；普通双引号、单引号 quote、反引号 quasiquote 均不改义。Reader、Zio reader、Tree-sitter injection、LSP 和文档同时迁移。

```lisp
;; 规划的原始多行字符串：JS 块仍是标准 JS
(inline js/engine {:phase :run :mode :function :bindings {:x 7}}
  #"JS
(x) => {
  const label = `value=${x}`;
  return { label, value: x * x };
}
JS"#
)
```

字符串本身不指定 JS 解析模式；语言来自 inline 引擎。编辑器可识别 literal engine，动态 engine 不保证静态 injection。错误记录 Zio 调用位置及外语块起始、外语行列，不能只显示宏生成代码位置。

### 7.4 执行限制

Zio fuel 不会自动限制外语执行。原生 JS 验证引擎中断、heap 限额与清理；浏览器外语不可信执行使用独立 Worker 并可终止，主页面 bridge 只给可信代码。Rust 等不能可靠中断的路径由进程边界限制，超时清理进程组、输出有界。网络／文件等权限由宿主授予，不由导入库或镜像自行取得。

## 8. 完整发行合同

默认开发包提供 `zio` 统一入口；内部允许多个二进制／资源，不强制一个巨大可执行文件：

```text
bin/zio, bin/zio-lsp（可供编辑器直接启动）
share/zio/libs, share/zio/langs（工具链源码与编译模块）
share/zio/grammar（Tree-sitter parser/query/source）
share/zio/wasm（browser 与 WASI 运行资源）
share/zio/docs, share/zio/examples
share/zio/engines（已捆绑引擎、ABI 资源与可选离线引擎包）
manifest.json, licenses/
```

发行清单列 version、commit、target、产物格式、组件版本／能力、文件 checksum、许可证、外部系统依赖。发布包必须明确实际支持目标，不能将 CI matrix 或交叉编译成功称为该平台运行验收。

原生 JS 随声明支持 JS 的默认开发包捆绑。Rust 编译支持由同一安装器安装可选离线引擎包（固定 rustc、sysroot、链接依赖及许可清单）；未装时明确指出缺组件，不静默调用系统 rustc。Zio 本身解释／编译／LSP／镜像／bin 构建不能依赖用户安装 Rust。跨目标构建所需 runtime 模板、sysroot 和 linker 分目标声明，不能下载到一半才发现缺依赖。

完整发行包包含所有 Zio 核心开发工具；可选大型外部 SDK／引擎包有离线整套安装路径，不把“可选”当成 Rust 集成未交付。Grove 单独发行／选择安装，语言包不默认捆绑 torch 或 Grove 业务。

## 9. 规划 CLI

以下是实现目标，不是当前用法；保留已公开脚本入口，内部统一到相同运行路径：

```sh
zio repl
zio run program.zio --engine interpreter
zio run program.zio --engine compiled
zio compile program.zio -o program.zioc
zio self-build --verify
zio image save session.zio -o session.zio-image
zio image inspect session.zio-image
zio image run session.zio-image
zio build program.zio --target native -o program
zio build session.zio-image --target wasm-browser -o web-app
zio build program.zio --target wasm-wasi -o program.wasm
zio lsp
zio engines list
```

`image save` 执行初始化程序并在退出求值后保存其 context；`inspect` 只读校验／元数据，不执行恢复器。初始化源码也是代码，不能认为 save 无副作用。run／build 入口显式，任何不支持目标、版本、外部资源或缺组件都非零退出并说明原因。

## 10. 验收矩阵

| 编号 | 必须观察的结果 |
| --- | --- |
| S1 | 已公开语义语料在 Rust 对照、Zio 求值器、编译执行的结果／错误／副作用一致；reader 来源位置可用 |
| S2 | Stage 1/2/3 规范化字节一致，编译后的 compiler 能编译真实程序；不是 hash-only 自证 |
| I1 | 新进程恢复闭包与共享可变绑定，保持循环、模块、宏和 ZOS 已有子集行为 |
| I2 | 非注册宿主 closure、活动资源、坏引用、超限、版本冲突均拒绝；失败不污染会话 |
| I3 | 无 codec 外部句柄拒绝；恢复不能继承构建权限／密钥；portable 镜像在声明目标恢复 |
| A1 | native 应用移出仓库且无 zio/rustc 开发安装时可运行，包含正确入口及真实动态依赖 |
| A2 | browser 与 WASI 两条路径真实运行，target import 不兼容在构建时拒绝 |
| E1 | 原生／浏览器 JS 的约定数据与对象语义、异常、编译调用一致；无限循环路径能被明确终止 |
| E2 | Rust 编译加载调用走固定工具链和 ABI；目标／签名不符拒绝，未授权编译不执行 |
| E3 | inline 参数不拼源码、不重复求值；原始块保留外语语法，错误能回到调用与块位置 |
| D1 | 一次安装含全部 Zio 开发组件，离线且无 checkout 使用；组件同版本，清单与许可齐全 |
| D2 | 实际语言服务会话、语法高亮、浏览器 WASM、WASI、native smoke 完成；应用不被误称为完整发行 |

执行工作包、文件落点和命令见[实施计划](../plans/2026-10-11-zio-language-toolchain.md)。语言／自举、镜像／导出、引擎／发行分开验收；不得将内部子集里程碑称为整项完成。
