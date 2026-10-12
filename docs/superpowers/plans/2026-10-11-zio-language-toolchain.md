# Zio Language, Images and Complete Toolchain Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Read the repository's GitNexus impact rules before changing any implementation symbol.

**Goal:** 交付用 Zio 实现的求值／编译工具链、可恢复镜像、独立 native/browser/WASI 应用、JS/Rust 引擎接入及一次安装配齐的开发发行版。

**Architecture:** 一份语言语义、两条实现主线、四类产物：完整开发发行版、编译模块、运行时镜像、应用产物。复用当前 Zio 编译器、Rust VM、来源位置、宿主授权、LSP 和发行脚本；Rust 保留机制层，Zio 承担语言策略与库接口。外部引擎先实现真实 JS/Rust 两条路径，再抽取二者需要的最小共享合同。

**Tech Stack:** Zio、现有 Rust workspace/serde/im、Astro、Tree-sitter、WASM/wasm-bindgen、WASI、嵌入式 JS 引擎、固定版本 rustc 与版本化 C ABI。

---

## 0. 执行合同

设计依据：[语言工具链设计](../specs/2026-10-11-zio-language-toolchain-design.md)。本计划取代旧文档中与其冲突的工具链、自举、镜像、引擎与发行排期；旧 Grove 合同及历史证据不删除。L00–L02 已按下列证据验收；未勾选工作包仍未执行，不是已完成声明。

这是依赖有序的工作包计划，不是把整门语言的一次性实现代码复制进 Markdown。每包先读列出的真实文件、做 graph impact、固定行为合同，再做最小实现；禁止用 stub、假引擎或只有加载成功的 smoke 交付工作包。新增文件路径是明确的设计落点，不代表当前文件已经存在。

每个包执行顺序：

1. Graph `context`/`impact` 和必要 LSP references，报告调用方、执行流及 HIGH/CRITICAL 风险；UNKNOWN 用来源检查补证。
2. 先保留消费可见的失败／回归用例，覆盖表内指定边界；不写源文本或转发 wiring 测试。
3. 实现并迁移全部调用方；新增模块的代码、格式与文档循现有模式。
4. 实跑表内 CLI／进程／浏览器／LSP 场景，保存真实输出；阶段末统一构建与测试，不在并行代理中重复跑全套。
5. 更新 feature matrix 与发布文档的状态及日期。未运行不写 passed。
6. 若用户要求提交，先 `node .gitnexus/run.cjs detect-changes --scope all --repo .`，partial/truncated 必须解决；本计划不自动提交／发布。

内部小步可以先实现子集，但工作包只有全部合同通过才完成。下面所有新 CLI 命令均为待实现接口；不能用当前 CLI 不识别命令作为已有行为验证。

## 1. 文件与责任图

| 责任 | 复用／修改现有文件 | 明确新增落点 |
| --- | --- | --- |
| 统一语义与来源 | `langs/core/src/{syntax.rs,span.rs,eval.rs,macros.rs,bootstrap.rs}`；`libs/std/core.zio` | `docs/language-reference.md`；`langs/core/tests/toolchain_contract.rs`；`examples/toolchain/semantics.zio` |
| Zio reader／求值器 | 现有 reader 作 Stage 0 对照 | `langs/interpreter/{reader.zio,evaluator.zio,expand.zio}`；`examples/toolchain/meta-eval.zio` |
| 自举编译 | `langs/compiler/compiler.zio`；`langs/core/src/bytecode.rs`；`langs/core/tests/self_hosting.rs` | `langs/compiler/analyze.zio`；`tools/smoke-self-build.sh` |
| 镜像机制 | `langs/core/src/{value.rs,env.rs,context.rs,module.rs}`；`langs/core/src/zos/{object.rs,class.rs,gf.rs}` | `langs/core/src/image/{mod.rs,wire.rs,save.rs,restore.rs}`；`langs/core/tests/image_contract.rs`；`examples/toolchain/image-session.zio` |
| CLI／应用导出 | `langs/cli/src/main.rs`；`langs/core/src/{lib.rs,bytecode.rs,wasm.rs}` | `langs/cli/src/{commands.rs,bundle.rs}`；`tools/smoke-toolchain-artifacts.sh`；`examples/toolchain/app.zio` |
| WASI | 当前 browser feature 与构建脚本 | `langs/wasi/{Cargo.toml,src/main.rs}`；`tools/build-wasi.sh` |
| JS 引擎 | browser bridge 与 NativeFn | `contribs/native/js/{Cargo.toml,src/lib.rs,src/values.rs}`；`libs/interop/js.zio`；`examples/interop/js.zio` |
| Rust 引擎 | 当前可信宿主／worker 边界 | `contribs/native/rust-engine/{Cargo.toml,src/lib.rs,include/zio_engine.h}`；`libs/interop/rust.zio`；`examples/interop/rust.zio` |
| 外语块 | reader、syntax、compiler、Tree-sitter、LSP | `libs/interop/inline.zio`；`contribs/tree-sitter-zio/queries/injections.scm`；必要 external scanner `contribs/tree-sitter-zio/src/scanner.c` |
| 完整发行 | `tools/install.sh`、`tools/build-wasm.sh`、`.github/workflows/release.yml`、`docs/release.md` | `tools/test-toolchain-distribution.sh`；`tools/install-engine-pack.sh` |

仅在责任已经超出现有文件时拆模块，不做无关结构重写。新 Rust workspace member 必须同时更新根 `Cargo.toml`/lock 与发布路径；不能另起运行时或字节码。

## 2. 依赖与主线

```text
L00 统一语义与实测基线
 ├─ L01 Zio reader/求值器/展开器 ─ L02 编译前端迁移与自举
 ├─ M00 对象身份与图格式 ─ M01 保存恢复 ─ M02 资源/权限/CLI
 └─ D00 完整发行装配（贯穿每包，不等待最终阶段）
L02 + M02 ─ A00 native 导出 ─ A01 browser 导出
                         └─ A02 WASI 导出
L00 ─ J00 native JS ─ J01 browser/compiled 一致性
L00 ─ R00 Rust 编译型引擎
J01 + R00 + L01 ─ E00 inline/原始块/工具支持
全部工作包 ─ D01 离线完整发行与全矩阵验收
```

L01 与 M00 可以在语义固定后并行；修改 `value.rs`/`context.rs`/`bootstrap.rs` 的包需要单一集成所有者。J/R 的引擎实现可并行，但共享传值与权限合同先由主代理固定。Grove 业务演化不阻塞本计划，不为了跑 Grove 顺手添加语言特殊形式。

## 3. 工作包

### L00：统一语言参考与三个执行面的基线

**Files:** 新建 `docs/language-reference.md`、`langs/core/tests/toolchain_contract.rs`、`examples/toolchain/semantics.zio`；修改 `docs/feature-matrix.md`、`apps/site/content/syntax/index.mdx`；读取现有 runtime/self_hosting 合同与 `libs/std/core.zio`。

- [x] 记录当前 reader/AST/VM 实际入口和已有特殊形式；修正参考中的示例，不同时发明新语法。规范 let/let*、cond 列表子句、nil/false、整数、rest、quote、quasiquote、模块与 source span。
- [x] 建立语义语料：作用域和变异、求值次序、闭包/HOF、尾递归、try/catch、宏子集、模块导出隔离、ZOS 子集、数值边界；AST 使用 `bootstrap::eval_source`，VM 使用 `bytecode::compile_source` + `execute_module`，比较返回值、错误类别/位置和记录输出。单独核对 `finally` 的实现状态，不把未实现语法写成现有能力；不能用当前回退 AST 的 `run_source` 冒充独立 VM 验证。
- [x] 新增可运行基线文件，使用以下核心表达式并打印确定结果；模块与对象例子复用现有契约，不靠计数宣称完成。

```lisp
(defn make-adder [k] (fn [x] (+ x k)))
(println ((make-adder 10) 5))
(println (let* [x 2 y (+ x 3)] (* x y)))
(println (let [x 3] `(+ ~x 1)))
(println (< 9007199254740993 9007199254740994))
```

Run: `cargo run -p zio-cli -- examples/toolchain/semantics.zio`。
Expected: `15`、`10`、`(+ 3 1)`、`true`，并按文件后续语料输出已规范的结果；`cargo test -p zio-core --test toolchain_contract` 比较现有两执行面。

**Exit:** 语言参考不再依赖 Rust 类型解释语义；全部已有公开能力有语料对应，工具链状态与源码一致。新增行为尚未实现时写明确目标，不改成已支持。

**L00 执行证据（2026-10-11）：**

- `cargo test -p zio-core -p zio-cli`：238 项通过；其中 28 项 toolchain 合同（26 项 AST／VM 对照、VM 深调用、跨 fresh context 产物可重复性）、24 项 runtime 合同、3 项 staged compiler 合同。没有调大栈、忽略测试或切换 main harness。
- 修正局部定义／捕获／前向绑定、匹配深度 unquote 内定义、编译宏及产物的原始源码版本。VM 普通调用使用显式调用帧，1000 层返回与跨帧 catch 可运行；cond 编译改为逐子句循环，100 个假子句后命中 true 保留结果 42。
- Stage 1／2 安装整个编译模块及辅助函数，规范 JSON 字节一致。语法键排序不依赖无序 metadata map 的 Display；同 datum 以源码位置破同序。此固定点仍依赖 Rust reader／宿主机制，不能代替 L01／L02 完整自举。
- 原生 CLI、序列化后迁入 fresh context 的 VM：`semantics.zio` 精确 16 行输出、返回 nil。当前核心重建的 WASM 在 Node 调用实际 `ZioSession` 得到同样输出；编译宏前向定义返回 7，100 子句 cond 返回 42。
- 语言参考、README、语法入口、架构／定位与特性矩阵同步；Astro 构建 88 页，HTTP 实读语言参考、语法入口及项目清单，核对参考链接、238 项通过证据与 541 项仓库测试清单。浏览器自动化 host 不可用，HTTP／WASM 运行不冒充 DOM／视觉验收。
- 两个代码审查切片检查词法／来源与 syntax／执行帧；发现的 unquote 扫描与同 datum 排序问题已用运行合同修正。外部委员会的两位成员分别因 401 鉴权／402 余额失败，不宣称外部共识。

**下一工作包：L01。** L00 固定的是现有语言语义与可复跑执行面；Zio reader／元循环 evaluator、镜像、外部引擎、应用导出和完整发行仍按后续工作包验收。

### L01：Zio reader、元循环求值与展开

**Files:** 新建 `langs/interpreter/reader.zio`、`evaluator.zio`、`expand.zio`、`examples/toolchain/meta-eval.zio`；修改 `bootstrap.rs`、`syntax.rs`、`toolchain_contract.rs`；读取 Rust reader/eval/macros/module/zos。

- [x] 用当前 Rust 引导读取 Zio interpreter 源码；Zio reader 输出现有 SyntaxNode 合同，保留来源、quote/generated/macrocall 区分。测试 Unicode byte offset、未闭合串/列表、奇数 map、dispatch、quasiquote。
- [x] Zio evaluator 显式携带环境和控制状态；eval/apply、词法 binding、set!、tail/recur、rest、异常、模块和已有 ZOS 子集依次迁入。宿主只提供值/环境/对象/I/O机制，不能把整段表达式交给 Rust eval 来伪装元循环。
- [x] Zio expand 实现当前 procedural/syntax-rules 语义与来源传播；不是只反复 macroexpand 最外层。将 L00 语料加入第三执行面。
- [x] 在 meta-eval 示例中用 Zio reader/evaluator 执行 L00 程序并输出同样结果；检测递归调用没有隐藏回到 Stage 0 eval。

Run: `cargo run -p zio-cli -- examples/toolchain/meta-eval.zio`；`cargo test -p zio-core --test toolchain_contract`。
Expected: 三执行面值/错误/副作用一致；未授权模块/I/O保持拒绝；大尾递归不依赖 Rust 栈增长。

**Exit:** 首轮全部已有公开语义覆盖。reader/evaluator 自身不是 Rust evaluator 的转发壳；同步 future/chan 语义保持现状，不冒充真实并发。

**L01 实施与验收记录（2026-10-11）：**

- 实现：`langs/interpreter/{reader,evaluator,expand}.zio`；显式公开入口为 `zio-meta-read`／`zio-meta-eval`，Rust 嵌入对应 `interpreter::{read_source,eval_source}`。默认 AST／VM 执行入口未切换。
- 机制桥在 `langs/core/src/interpreter.rs`，并更新 bootstrap、EvalEngine/native/macro gateway、严格 SyntaxNode 原子验证及 SourceMap UTF-8 span 校验。Rust 仅引导安装实现、提供值／环境／对象／宿主与现有编译产物执行；目标 reader/evaluator/宏展开不是 Rust 全表达式转发。
- 最终 `cargo test -p zio-core -p zio-cli --no-fail-fast`：**258 passed，0 failed**。包括 29 项 toolchain（含三面行为对照与 VM 专项制品合同）、17 项元循环、2 项 syntax atom、24 项 runtime、3 项 staged compiler 合同；不是全 workspace 的 561 项实跑声明。
- 元循环合同验证 Unicode 字节位置、reader 错误、同名源码不同版本、词法与前向定义、普通／尾调用、宏来源／卫生子集、ZOS 方法组合、同步 future/promise/chan、模块失败／循环／导出与 root 授权。禁止目标源码 AST observer 事件并通过 native probe 检查函数、回调、宏、ZOS 方法、模块初始化保持 MetaEngine。
- 深度合同：10000 次直接尾调用、1000 层非尾返回／错误回退、1000 层递归 map／reduce、3000 次尾部 apply，未增大 Rust 栈或绕过断言。内置 map/filter/reduce/apply/sort/future-call 使用显式控制帧；普通 HOF 仍拒绝 macro 值，按原 native 身份而非名字识别 intrinsic。
- `cargo run -p zio-cli -- examples/toolchain/meta-eval.zio` 已实跑并逐行比对 L00 **16 行输出**。`bash tools/build-wasm.sh` 后在 Node 调用实际 ZioSession：同一元循环基线返回 nil、精确 16 项 console 输出；递归 map 1000 返回 1000；`::use`／`::export` 与 AST 同为 `[:core :answer]`；浏览器宿主拒绝文件写入。
- 四片源码审查（reader、展开、控制状态、机制桥）已收敛；修复 syntax-rules 替换布尔 map 键合并、字符串 helper 错用 slice、动态 reader 错误位置、HOF callback Rust 栈递归与包 clause 双冒号兼容性。没有扩大当前宏／并发语义来迎合错误测试假设。
- 文档／站点状态同步；`tools/project-status.sh --check` 通过，Astro 构建 **88 页**，HTTP `/syntax/` 已读到新入口。无已连接浏览器，不声明 DOM／视觉验收。

**保留边界：** Rust 引导与机制仍存在；环境句柄记录随创建数量增长，不宣称常量空间或速度提升，外部同步 native callbacks 仍可能重入宿主栈。完整 Stage 1/2/3 编译器自举属于 L02，镜像／应用导出／外语引擎／全套发行按后续工作包验收。


### L02：Zio 编译前端与真实固定点

**Files:** 修改 `langs/compiler/compiler.zio`、`langs/core/src/bytecode.rs`、`langs/core/tests/self_hosting.rs`、CLI；新建 `langs/compiler/analyze.zio`、`tools/smoke-self-build.sh`。

- [x] 将展开、绑定分析、尾位置、capture/global 选择迁到 Zio 前端；保留 Rust 指令执行、wire 校验、float bits 等机制。现有模块格式复用，实际版本变化才升级。
- [x] CLI 实现 `zio self-build --verify`，记录 Stage 0/1/2/3 入口、规范化文件与真实 byte equality；不要只比较 FNV hash。compiled compiler 必须再次编译程序并执行已知答案。
- [x] 自举 smoke 在新的 context 运行编译器产物，不依赖同一 context 残留 interpreted compiler；证明前端读取/展开路径走 Zio。

Run: `cargo run -p zio-cli -- self-build --verify`；`cargo test -p zio-core --test self_hosting`。
Expected: Stage 1/2/3 normalized bytes equal；known-answer 程序的 sum=10、closure=15，来源错误一致；损坏指令／常量版本拒绝。

**Exit:** S1/S2；README 仅在真实输出后更新“首轮工具链自举已验收”。不得把完整 MOP、JIT 或编辑器自托管写进该完成声明。

**L02 验收证据（2026-10-11，Linux x64）：**

- `cargo test -p zio-core -p zio-cli --no-fail-fast`：259 项通过，0 失败；其中 self-hosting 4 项、toolchain 29 项。新增 Stage 3 合同先失败，再由真实三代构建通过。
- `bash tools/smoke-self-build.sh` 调用实际 CLI：Stage 1/2/3 各 739890 字节，直接比较规范 JSON 及落盘文件；两个 `cmp` 通过。三代 fresh context 均确认 `zi-read`、`zi-expand`、`zi-apply`、`zio--compile` 为编译函数，最后编译执行得到 sum=10、closure=15。
- 前端完整包含 reader/expand/evaluator/analyze/compiler；目标读取使用 `zi-read`，展开使用 `zi-expand`，删除旧 `compiler/expand` 和无效编译占位分支。模块格式仍为 v1，没有为状态变化虚增 wire 版本。
- 执行 observer 拒绝前端 helper 与目标 Stage0 AST 回退；缺失 capture helper 的制品不能借用旧解释定义。保护回调按登记的实际函数身份授权，不信函数名或可伪造 span。
- 源码版本与 UTF-8 byte span 经 fresh-context 序列化执行保留；宏错误保留定义来源；坏模块版本、opcode 与常量类别拒绝。
- `tools/build-wasm.sh` 重建实际站点 WASM；Node `ZioSession` 用 Zio reader/compiler 安装并执行编译宏，sum=10、closure=15，元循环 1000 次尾调用返回 1000。
- Astro 构建 88 页；状态清单为 562 项仓库测试（不是全 workspace 实跑），`project-status.sh --check` 通过。HTTP 检查语言参考、语法、介绍、贡献与状态页，并确认服务 WASM 与重建字节相同；浏览器 DOM/截图确认自举命令段落渲染。此文档视觉检查不代替浏览器完整自举或应用导出验收。

**保留边界：** Stage0 引导解析及 Rust VM/值/环境/宿主机制仍在；默认源码执行器不变。便携编译器自举不是 Rust-free、优化 ZIR/JIT、完整 MOP、镜像、应用导出、外语引擎或全套发行。


### M00：对象身份、绑定单元与图格式

**Files:** 修改 `value.rs`、`env.rs`、`context.rs`、`module.rs`、`zos/object.rs`、`zos/class.rs`、`zos/gf.rs`；新建 `langs/core/src/image/{mod.rs,wire.rs}`、`langs/core/tests/image_contract.rs`。

- [x] 列出各现有 Value variant 的可保存规则：纯值、环境/函数/宏/类/模块/Buffer、原生 binding、需恢复资源、默认拒绝资源。先固定 ZOS 实例的可观察共享/clone规则；不靠序列化更改原语言的身份语义。
- [x] wire 记录 header(format/runtime/profile)、roots、nodes(id/kind/payload)、refs、modules/bindings 和资源需求；限制节点/边/字节，所有索引与 kind 校验。已存在 portable module 以引用使用而不是复制另一份指令格式。
- [x] 内存对象记录 ID，环境/closure/mutable cell 可形成环；测试同一闭包环境两次引用是一个节点，非同一环境不合并，错引用不被接受。
- [x] 不建立全局万能序列化框架，只为真实 runtime variants 定义编码；外部 codec 注册仅在 M02/E 包需要时扩展。

Run: `cargo test -p zio-core --test image_contract`。
Expected: identity/shared-cell/reference-boundary 行为满足合同；坏 kind、孤立引用、重复 ID、超限全部为命名错误，不 panic/分配无界内存。

**Exit:** 可编码运行时图，不是递归 JSON 值复制。M00 仅是格式/身份前提，不宣称镜像交付。

**M00 验收证据（2026-10-11，Linux x64）：**

- `image::encode_graph(roots, &ImageLimits)` 编码 18 种 Value variant：纯值内联按出现克隆；env/function/macro/buffer/cell/class/module 与显式 identity 的 ZOS 实例按真实指针/identity intern；NativeFn 只记注册名（计数 id 不进格式）并登记 `native-binding` 资源需求；future/channel 按对象路径命名拒绝；未知 ZOS object 类型拒绝。
- 编译函数引用 `ModuleArtifact::to_wire_json()` 的 module 节点一次，不复制指令；捕获是共享 cell 节点，未绑定保留单元命名拒绝。
- `image_contract.rs` 13 项合同全绿：同环境两引用一节点、克隆闭包 intern 一次、等价环境不合并、共享 buffer 一个 cell、let 自引用 env↔fn 环闭合、两次编码 canonical 字节一致、ZOS identity=7 共享而匿名实例克隆。
- wire 校验拒绝：未知 kind、越界引用、非 dense/重复 ID、kind/payload 类型错、坏 format/version、节点/边/字节/嵌套预算，全部命名错误；引用遍历为迭代式，不受 payload 深度攻击。
- 默认 `max_depth=256`：深嵌套 Value 的 encode/drop 递归在 2 MiB 测试线程上本就不安全（L00 quoted-list 已知限制），预算低于该界限；迭代式编码器是后续提升路径。
- 环境父链、模块表 exports、source map 与两遍建图恢复属于 M01；本包只固定图格式与身份前提。

### M01：完整静止镜像保存与恢复

**Files:** 新建 `langs/core/src/image/{save.rs,restore.rs}`、`examples/toolchain/image-session.zio`；修改 env/context/module/zos 实现与 `image_contract.rs`。

- [x] Save 遍历显式 roots，编码环境父链、闭包/宏源码或编译 body、捕获、module exports、ZOS 子集与 Buffer；记录 source map，排序语义无序表产生确定格式。NativeFn 只允许绑定稳定 ID/版本，不能保存指针或当前计数 ID。
- [x] Restore 先有界校验，再两遍建图/连边，在新 context 里提交完整根；不能边加载边污染旧会话。缺 native binding 或版本不符失败。
- [x] 镜像初始化例子创建两个捕获同一可变 binding 的函数，保存后新进程调用 setter 与 getter 检查共享；覆盖自引用环境、宏展开、模块 exports、类方法 dispatch 和 Buffer 修改。
- [x] 非注册 Rust closure、活动 channel/调用、未完成任务明确报对象路径；不能 silently nil。保存静止时刻，不序列化执行栈。

Run: `cargo test -p zio-core --test image_contract`；M02 CLI 落地后通过其新进程 smoke 补足本包端到端验收。
Expected: 恢复前后 getter/setter 与宏/模块/ZOS行为一致，重复引用保持共享，坏镜像不会改旧会话。

**Exit:** I1/I2；只有在 M02 新进程证明后才能对外宣称可恢复镜像。

**M01 验收证据（2026-10-12，Linux x64）：**

- `save.rs`：`save_image(ctx, roots, limits)` 遍历显式 roots、环境父链与模块表（按 display_name 排序、exports 排序、source 名），两次保存 canonical 字节一致。
- `restore.rs`：先 `wire::validate` + runtime 版本核对 + native binding 按名预检，再建图、连边（env/cell 先预留后统一填充），最后才把模块注册进目标 context——失败不留半成品；模块环境在连边阶段前预留。
- 内联标量携带 kind（string/symbol/keyword/char）、内联序列携带 seq（list/vector）——M00 初版丢掉的类别在 M01 补回并有合同覆盖。
- 22 项合同全绿：共享可变 binding（setter/getter 跨镜像）、自引用 env（f 绑定即同一对象）、宏在新 context 展开、模块 exports 恢复后可查、类方法 dispatch 恢复后可用、Buffer 恢复为一个共享 cell、缺 native binding 与 runtime 版本不符均为命名错误、示例 `examples/toolchain/image-session.zio` 端到端往返。
- 保留边界：闭包 body 经 Value 往返（无 span，源码表未入镜像）；编译闭包按 module 名恢复且要求有函数名；执行栈、future、chan 按路径拒绝；函数值直接成环（不经 env）报告具名错误。可恢复镜像的对外宣称仍等 M02 新进程 smoke。

### M02：镜像 CLI、恢复授权与资源边界

**Files:** 修改 CLI、`image/mod.rs`、bootstrap/context；新建 `langs/cli/src/commands.rs`、`tools/smoke-toolchain-artifacts.sh`，扩展 image tests。

- [x] 实现 `image save/inspect/run` 与 `compile ... -o ...`；inspect 只校验元数据，不运行恢复器。入口解析保存与运行路径复用，不遗留第二套 context 装配。
- [x] 宿主注册 trusted restorer(binding ID/version/capabilities)，资源描述不含密钥或授权值；缺 grant 先拒绝，再创建资源。JS handles 无codec时拒绝，后续 codec 同样需要验证和 grants。
- [x] smoke 启动进程 A save、进程 B inspect/run；恢复环境从宿主 grant 建立，移出源码树验证 portable 根。通过 canary 文件和环境 token 证明保存/恢复未继承权限或泄漏密钥。

规划 smoke 命令：

```sh
zio image save examples/toolchain/image-session.zio -o /tmp/session.zio-image
zio image inspect /tmp/session.zio-image
zio image run /tmp/session.zio-image
```

Expected: inspect 显示格式/目标/roots/required capabilities；run 能调用恢复的闭包和宏。无授权、缺 binding、无外语 codec 返回非零并包含对象路径/原因；failed restore 不破坏其他会话。

**Exit:** I1/I2/I3；资源复建至少跑通一个受 root 限制的实际文件资源例子，不能用 mock echo 验收恢复权限。

**M02 验收证据（2026-10-12，Linux x64）：**

- `langs/cli/src/commands.rs`：`image save/inspect/run` 与 `compile -o`；save 与 run 共用 `script_context` 装配（程序目录 + ZIO_PATH 的模块根），inspect 只做 `wire::Image::parse` 元数据校验，绝不触发恢复。
- 恢复授权：restore context 使用 `GrantIoHost`（stdio 透传，所有文件操作须落在 `--grant-dir` canonical 根内），模块根为空；镜像 resource requirements 在建图前按名预检（缺 binding 先拒绝再创建资源）。镜像格式只保存注册名，不含密钥或授权值。
- `wire::Image` 新增 `root_names`（与 roots 等长、唯一），`save_named_image` 供宿主命名；恢复侧按名绑定。
- `tools/smoke-toolchain-artifacts.sh` 实跑通过：进程 A save（538764 bytes，1 root，350 nodes）→ grep 证明 env token 与 canary 路径均未进入镜像 → 进程 B 在源码树外 inspect（format/root/module 均报告）→ run 恢复后调用闭包得共享绑定 1 → 无 grant 读 canary 被拒且报错点名 grant 边界 → `--grant-dir` 后读取真实文件资源成功 → compile 产出 14996 字节便携模块。
- `image_contract.rs` 新增 root names 往返/重复名/错长合同；23 项全绿。
- 保留边界：只保存静止时刻，执行栈与活动资源按路径拒绝；恢复不做外部资源自动重建（文件资源以宿主 grant 现场创建）；应用打包导出属 A00–A02。

### A00：原生独立应用

**Files:** 新建 `langs/cli/src/bundle.rs`、`examples/toolchain/app.zio`；修改 CLI/Cargo/bootstrap/bytecode；扩展 artifact smoke。

- [x] build 输入支持源码、`.zioc` 与 `.zio-image`。采用预构建 runtime 模板附带有界的 payload 和校验 footer；启动时校验 offset/length/version/entry。不运行 rustc 编译用户 Zio 程序，也不硬编码 checkout 路径。
- [x] entry 与依赖 manifest 明确；源程序 default 为顶层执行，镜像由显式 entry 启动。静态 modules 嵌入到 BufferIoHost 或已有 module loader profile，所有资源解析以包位置/host grant为准。
- [x] 对动态 eval/require 包含必要 reader/compiler/declared modules，缺封闭依赖时保守保留，不输出运行到一半才缺功能的程序。
- [x] 从源码、编译模块和镜像分别导出应用；放到临时目录，隐藏 checkout，PATH 无 zio/rustc，验证结果和动态行为；损坏 payload 拒绝。

Run: `zio build examples/toolchain/app.zio --target native -o /tmp/zio-app`；`/tmp/zio-app`。
Expected: 例子打印已知 `49` 并成功调用显式动态 eval 得到 `50`；镜像版还保持 M01 状态，外部依赖完整声明。

**A00 验收证据（2026-10-12，Linux x64）：**

- `langs/cli/src/bundle.rs`：应用 = runtime 二进制 + JSON payload + 36 字节 footer（magic/版本/长度/offset/FNV-1a 校验和）；启动钩子检测 footer 并运行 payload，普通 runtime 行为不变。构建只用当前二进制作模板，不调 rustc、不写死 checkout 路径。
- 三种输入：`.zio` 顶层执行、`.zioc` 便携模块（构建期即 deserialize 验证）、`.zio-image` 需显式 `--entry` 表达式，恢复后按名绑定 roots。
- 模块声明闭包：构建期用真实 reader 扫描入口与各模块源码的 `require`/`module` 形式，缺失依赖在构建时拒绝并指名（inline `(module :name …)` 自满足）；运行期按 manifest 顺序预注册，`require` 命中已加载模块，未声明依赖无法触碰 checkout。
- `bundle_contract.rs` 4 项全绿 + smoke 扩展：隔离环境（PATH 仅 /usr/bin:/bin、无 checkout）运行源码应用输出 49 与动态 eval 的 50；镜像入口应用输出 1（M01 共享绑定状态保持）；单字节篡改载荷被“checksum mismatch”拒绝且无输出；未声明模块的构建失败且不留下产物。
- 保留边界：应用为单文件 runtime+payload（非机器码 AOT）；浏览器与 WASI 导出属 A01/A02；bundle 不含 LSP 等开发工具。

**Exit:** A1。标注 bytecode/image+runtime，不宣称 machine-code AOT/native-speed。

### A01：浏览器 WASM 应用与镜像恢复

**Files:** 修改 `langs/core/src/wasm.rs`、`langs/cli/src/bundle.rs`、`tools/build-wasm.sh`；新建 browser artifact smoke 页面资源随包生成，扩展 `tools/smoke-toolchain-artifacts.sh`。

- [x] 导出 WASM runtime/payload 和 JS glue loader；不把 AST 浏览器 playground 当 compiler codegen。共享 `.zioc` 和 portable image loader，加载校验与来源错误保持一致。
- [x] 构建 manifest 指定 browser imports/entry/resources。DOM/JS 功能在此 profile 显式；未支持 native资源构建时拒绝。
- [x] 真实浏览器加载导出包，执行源码版49/动态50和镜像闭包/宏/ZOS probe；观察界面结果与console。把包移动到另一目录/URL后重复，证明不依赖仓库/固定资源绝对路径。

Run: `zio build examples/toolchain/app.zio --target browser -o /tmp/zio-web/app.html`；本地静态服务打开包生成的入口页面。
Expected: 实际 WASM 运行输出已知值，恢复 shared state，不调用站点旧 WASM；非法镜像/资源目标报明确错误。

**Exit:** A2 browser 子项和 I3 browser；浏览器主线程运行可信代码安全说明保留。

**A01 交付证据（2026-10-12，Linux x64）：**

- bundle 格式下沉到 core（`langs/core/src/bundle.rs`）：payload 记录、footer 读写校验、`BundleIoHost`（文件只存在于包内，stdout/console 透传）与 `run_payload(io, payload)` 由原生 CLI 与 WASM 共用；CLI 只保留构建编排。
- `ZioSession.run_bundle(payload)` 暴露给浏览器；`zio build --target browser` 产出单文件 HTML（glue URL + 内联 base64 payload，同一 payload 合同，无第二套 loader）。
- 合同：`bundle_contract.rs` 5 项全绿，其中浏览器包由 `tools/a01-browser-harness.mjs` 以 web-target glue + 模块字节（即页面加载的真实引擎）运行，程序 console 输出 49 与动态 eval 的 50；镜像入口在原生侧输出 1（恢复状态保持）。包移动到任意目录后行为不变（只引用 glue URL）。
- 已知限制：本次浏览器自动化 host 无法访问本地服务（ERR_CONNECTION_REFUSED），未做浏览器 DOM/截图验收；node 通道使用与页面相同的 wasm 二进制与 API 调用路径，不冒充浏览器渲染验收。

### A02：WASI 应用与 portable 镜像

**Files:** 新建 `langs/wasi/{Cargo.toml,src/main.rs}`、`tools/build-wasi.sh`；修改 root workspace、bundle、feature configuration；扩展 artifacts smoke。

- [x] 用 `wasm32-wasip1` 构建只依赖 WASI imports 的 runtime；不打开 browser `wasm` feature，避免 js-sys/globalThis 混入。真实运行器以固定版本作为验收工具，不是用户应用隐含依赖。
- [x] stdin/stdout/文件访问接现有 IoHost 与显式 preopen/grant。portable image 有相同闭包/模块结果；DOM、不可移植引擎／native资源在 build 阶段拒绝。
- [x] 源码/模块/镜像三输入导出 WASI，使用没有 preopen 的运行器执行；需文件例子只在显式 preopen 下成功。

Run: `zio build examples/toolchain/app.zio --target wasm-wasi -o /tmp/zio-app.wasm`；`wasmtime run /tmp/zio-app.wasm`（验收环境安装已锁定版本）。
Expected: 已知49/50与镜像共享状态结果一致；无preopen文件访问拒绝；browser JS全局依赖不能生成此目标。

**Exit:** A2 WASI 子项与 I3 WASI；WASM模块需要WASI宿主应在发行清单声明，不能叫本机裸bin。

**A02 交付证据（2026-10-13，Linux x64，wasmtime-cli 24.0.0）：**

- `langs/wasi`（`zio-wasi`）：只依赖 `zio-core`（`default-features = false`，不打开浏览器 `wasm` feature）；`build.rs` 按 `ZIO_WASI_PAYLOAD` 把 payload 编译进模块——WASI 无法读取自身文件，这是单文件 `.wasm` 的唯一诚实路径。`tools/build-wasi.sh` 构建运行时模板（43 KB）。
- 真实运行：`wasmtime run /tmp/zio-app.wasm` 输出 49 与显式动态 eval 的 50；镜像入口 `((get root-0 :bump))` 输出 1（恢复状态与原生一致）。模块 imports 仅 `wasi_snapshot_preview1`（0 处 js-sys/globalThis/wasm-bindgen 字符串）。
- 授权边界矩阵：无 preopen 无 grant → 按 grant 边界指名拒绝；preopen 无 grant → 仍拒绝；preopen + `--grant DIR` → 读到真实文件内容。包内文件优先，宿主文件只经显式 grant 透出（`run_payload_with(host, payload, true)`；CLI/wasm 保持 `run_payload` 的封闭语义）。
- 构建期拒绝：入口或声明模块深层使用 `js/eval` 等浏览器专有符号 → 以 “browser-only … a WASI application cannot” 拒绝，且不留产物。
- `bundle_contract.rs` 9 项全绿（原生 4、浏览器引擎 1、WASI 运行 3、拒绝 1）；缺 wasmtime/target 的环境按原因跳过而非假通过。
- 边界：per-app 重编译换单文件（链接缓存后 ~30s）；grant 采用词法归一化（wasi-libc canonicalize 在普通 preopen 下不可用），符号链接逃逸为已知限制。

### J00：原生嵌入 JS 引擎

**Files:** 新建 `contribs/native/js/{Cargo.toml,src/lib.rs,src/values.rs}`、`libs/interop/js.zio`、`examples/interop/js.zio`；修改 workspace/CLI装配；读取现有 wasm bridge。

- [ ] 实际验证 QuickJS 系列原生引擎的版本/维护/许可证/目标/中断/heap cap/回调，再锁定绑定依赖；若候选不满足合同，提交实际失败证据并选等价可嵌入引擎，不以mock替代。
- [ ] 引擎实例的 eval/load/call/property 操作经已安装绑定，Zio库包装；句柄记录所属实例、身份与生命周期。默认不给FS/network/process。
- [ ] 规定i64安全整数/BigInt转换、undefined/null区别的显式表示、非有限数、string/keyword key冲突拒绝、循环外语object句柄与JSON数据转换边界；回调同步/Promise显式，不默认await。
- [ ] smoke 执行Math.max、返回对象并修改属性、方法this、Zio回调、抛错、BigInt边界、Promise显式调用；无限循环被引擎中断，heap cap触发明确错误，context之后可重用。

Run: `zio run examples/interop/js.zio --engine interpreter`；同文件 compiled模式在J01通过。
Expected: JS返回49/可变对象正确，`9007199254740993`不变成另一整数；跨实例handle拒绝；未授权FS调用拒绝；无限循环不会挂死主进程。

**Exit:** native E1，随发行包真实捆绑引擎。核心只增加必要opaque机制，不引入Node/DOM业务语义。

### J01：浏览器 JS 与编译执行合同统一

**Files:** 修改 `langs/core/src/wasm.rs`、`eval.rs`、`bytecode.rs`、`libs/interop/js.zio`、工具链合同；维护现有site与Grove消费者的调用。

- [ ] 已安装库提供显式函数解析，替换仅AST解释可查找的 dotted hook；所有已用 `js/*` 调用迁到编译与解释共用路径，移除旧resolver而非保留第二默认语义。
- [ ] 原生/浏览器均执行相同纯JS语料；browser extra DOM单独注明不可移植。引擎、函数句柄、exception/source provenance共用语言接口。
- [ ] browser可信page bridge保留；不可信JS在Worker上执行、超时terminate，不声称页面eval安全。应用若要DOM权限，清楚地成为可信代码路径。
- [ ] 在真实浏览器上分别解释和编译使用对象/this/回调/Promise，返回精确结果；在原生compiled模式重复。

Run: `zio run examples/interop/js.zio --engine compiled`；浏览器加载A01导出的互操作程序。
Expected: 同合同的四组合结果一致；任一执行面不再依赖独有symbol hook；Worker无限循环可以终止；JS handle无codec镜像明确拒绝。

**Exit:** E1完整。用户当前页面/Grove路径不因为cutover失去DOM调用。

### R00：Rust 编译、加载与稳定 ABI

**Files:** 新建 `contribs/native/rust-engine/{Cargo.toml,src/lib.rs,include/zio_engine.h}`、`libs/interop/rust.zio`、`examples/interop/rust.zio`、`tools/install-engine-pack.sh`；修改 workspace/发行清单/bundle。

- [ ] ABI用版本化C入口，包含module版本、函数表、状态/错误和有界typed payload；跨边界不传Rust Value/Vec/trait对象。buffer由分配方提供显式release，panic不跨ABI unwind，签名和target不符加载前拒绝。
- [ ] 编译从宿主认可的离线Rust引擎包选择固定rustc/sysroot/linker，argv/cwd/env清理；源码不能指定执行文件、搜索PATH、任意build.rs/proc-macro。受信Rust编译与不可信worker编译分开。
- [ ] 编译真实 `square(i64)`导出并调用得到49；错误编译显示Rust源位置＋Zio调用位置。运行时加载原生module必须明确grant，未授权不编译/加载。
- [ ] 应用携带生成module，构建阶段使用rustc但native app运行不需rustc；运行时编译能力若明确请求则带引擎包。跨目标未提供工具链时build阶段拒绝。执行不可信Rust用可终止隔离进程，验证panic/timeout/输出边界。

Run: `zio run examples/interop/rust.zio --engine compiled`；`zio build examples/interop/rust.zio --target native -o /tmp/zio-rust-app`；离线执行应用。
Expected: 49，版本/target错误拒绝；编译器失败非零；无grant不会启动rustc；系统PATH可疑同名rustc不被执行。

**Exit:** E2；离线engine pack与license齐备，不能只检查系统rustc存在就宣布交付。

### E00：inline、原始源码块和语言工具支持

**Files:** 新建 `libs/interop/inline.zio`、`contribs/tree-sitter-zio/queries/injections.scm`、必要`src/scanner.c`；修改 Rust/Zio reader、syntax/span、compiler、Tree-sitter grammar/corpus、LSP analysis/protocol、语法文档与API注释。

- [ ] 按规格实现 `(inline engine options source)` 宏与运行函数，options明确phase/mode/bindings。run绑定求值一次且通过参数传值；build拒绝运行时局部值，只接显式构建输入。不将Zio值拼入外语字符串。
- [ ] `#"TAG` fenced原始字符串具有固定起止规则、冲突换tag、无转义/无插值、来源跨度；结束行后才能跟其他Zio形式。Rust和Zioreader同时支持。单引号quote、反引号quasiquote和JS模板不改义。
- [ ] Tree-sitter external scanner保存delimiter状态支持增量编辑和未闭合输入；已知literal引擎提供injection，动态引擎不伪装已知语言。LSP只分析，不执行JS/Rust源码。
- [ ] 跑JS expression/function/module和Rust支持的module build组合；错误阶段/mode拒绝，raw源码里的引号/括号/`${}`保持原样，绑定副作用只执行一次。

规划调用：

```lisp
(inline js/engine {:phase :run :mode :function :bindings {:x 7}}
  #"JS
(x) => ({label: `value=${x}`, value: x * x})
JS"#
)
```

Run: reader/CLI语料、`tree-sitter test`（cwd `contribs/tree-sitter-zio`）、`python3 tools/lsp_probe.py target/debug/zio-lsp`、真实编辑器/浏览器源码错误例子。
Expected: 外语结果value=49、label="value=7"；未闭合块报起始位置，JS语法错给块内行列与调用位置；LSP处理文件不产生源码副作用。

**Exit:** E3；所有syntax消费者迁移，无临时reader shim或只Rustreader支持。

### D00：默认完整开发发行装配

**Files:** 修改 `tools/install.sh`、release workflow、docs/release、CLI command dispatch；新建 `tools/test-toolchain-distribution.sh`，引擎离线安装器来自R00。

- [ ] 从L00起维护发行清单；随工作包实际产物增加解释器/编译器源码与模块、LSP、Tree-sitter parser/query、browser/WASI、stdlib/examples/docs/licenses；仅有源码不等于已提供可运行组件。
- [ ] `zio lsp`委派包内同版本LSP，保留编辑器可直接启动`zio-lsp`；`zio engines list`读取实际组件能力。应用Grove独立可选安装，默认语言包不再依赖torch/Python环境。
- [ ] 修正installer profile/target输入与CI真实二进制路径、Windows.exe/shell入口；assembly使用已构建目标，不能交叉构建完又隐式重建host。清单checksum覆盖实际组件和目标。
- [ ] 预构建native/browser/WASI runtime模板随包；本机Zio build不需用户rustc。默认JS打包；Rust可选离线引擎包使用同一安装流程，不把可选变成未实现。

Run: `ZIO_PROFILE=release tools/install.sh /tmp/zio-dist`；`tools/test-toolchain-distribution.sh /tmp/zio-dist`。
Expected: 无checkout、无网络且PATH无开发工具时 REPL/run/compile/image/build/lsp/engines可用；每个必需组件缺失都安装失败，不警告后继续。

**Exit:** D1，默认开发包完整；不承诺运行任意未声明目标/SDK。

### D01：全产物与跨目标发布验收

**Files:** artifact/distribution smoke、release workflow、feature matrix、release/roadmap/reference/site。

- [ ] 完成S1/S2/I1/I2/I3/A1/A2/E1/E2/E3/D1/D2证据表，每行记录commit、target、命令和真实观察。缺一项不发布“完整工具链交付”。
- [ ] 在每个实际声明支持的平台运行native app与语言服务，不能以cross-build或CI matrix代替runtime；浏览器/WASI独立验证，应用包移动目录后仍运行。
- [ ] 用发布归档解压后的路径跑LSP initialize/didOpen/diagnostics/shutdown和语法注入；用源码及镜像分别生成native/browser/WASI应用，确认无开发安装依赖。
- [ ] 校验离线Rust engine pack和默认JS许可、组件版本/清单/checksum；运行全workspace suite与Astro构建，观察实际站点文档和新语法示例，不把仅构建当浏览器行为验收。

Run: `cargo test --workspace`；`cargo test -p zio-core --test self_hosting`；`tools/smoke-self-build.sh`；`tools/smoke-toolchain-artifacts.sh`；`tools/test-toolchain-distribution.sh /tmp/zio-dist`；`bun run build`（cwd `apps/site`）；真实浏览器及WASI运行器。
Expected: 每目标已知程序值/错误、恢复状态、引擎拒绝路径、LSP和文档入口满足对应规格。发布之前执行graph detect-changes并经用户授权。

**Exit:** 所有规格验收，不留“以后补”的核心产物。更完整MOP/JIT/Node兼容属于独立后续需求，不阻碍此明确合同交付。

## 4. 规格覆盖与交付顺序

| 要求 | 工作包 | 交付依据 |
| --- | --- | --- |
| 语义独立、整洁统一语法 | L00/L01/E00 | 语言参考＋三执行面一致，reader/tooling同步 |
| 元循环求值器 | L01 | Zioreader/evaluator实际执行，不回调整段Rust eval |
| 自举编译器 | L02 | 多代字节相等＋真实已知答案程序 |
| 镜像图/共享/循环/模块/ZOS | M00/M01/M02 | 新进程恢复和可观察行为 |
| 资源权限与坏输入拒绝 | M02/J00/J01/R00 | canary、边界、timeout、失败隔离 |
| bin应用 | A00 | 无checkout/开发安装独立运行 |
| browser/WASI应用 | A01/A02 | 两profile分别实跑，portable镜像恢复 |
| 解释型与编译型外部引擎 | J00/J01/R00 | 实际JS引擎与Rust编译模块 |
| inline与原始代码块 | E00 | 阶段/绑定/异常/source/tooling一致 |
| 全套默认发行与可选离线SDK | D00/D01 | 安装归档全组件，同版本，无网络依赖 |

首个执行批次为 **L00 + D00现有组件装配核对**，随后 L01/M00并行。不得在语义和镜像身份未固定时先实现花哨inline语法，也不得以重写Grove替代语言交付。

## 5. 文档维护与当前验证范围

用户文档入口：README、语言介绍、语法参考、库参考、互操作说明、release。设计/实施/历史证据放贡献文档；能力状态只按实际工作包证据更新feature matrix。每包新增公开函数以源码`;; doc:`或Rust文档说明边界，不重复手写第二份库API。

本轮仅更新文档与计划，验证本地Markdown链接、方案覆盖和Astro生成；不声称本轮实现／运行新CLI、镜像、引擎或导出。构建输出与页面检查结果由本轮交付报告记录，不预填passed。
