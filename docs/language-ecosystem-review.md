# Zio 语言设计与生态评审

日期：2026-10-15。评审基线：`acdbd48cca70cac6301172442a101a61623b8134`。

这是基于当前规范、源码和合同测试的设计评审，新增建议尚未采纳，不改变现行语言语义。当前行为仍查阅[语言参考](language-reference.md)，能力成熟度查阅[特性矩阵](feature-matrix.md)。本次未重新执行语言运行时测试；下文的历史验收来自已有工作包记录，源码和测试存在不代表本次运行通过。

配套：[设计决策清单](language-design-decisions.md) · [生态路线图](ecosystem-roadmap.md) · [交互式决策图（本地 HTML）](design/zio-design-decisions.html)。HTML 可独立打开；其 JSON 位于同目录，可重新生成。HTML 不由当前 Markdown 文档路由自动发布。

后续用户补充：AI 可理解/执行/探索、Common Lisp condition/restart、help/函数查询、语言执行 harness 与解释器 MCP 服务，已整理为[AI 交互设计](ai-language-interaction.md)。方向获用户确认，接口仍为草案；restart 的最小核心提升到早期主线。

后续 Groiz 对照与持续自我进化目标见[专项评审](continuous-evolution-design.md)：现有方向相容但不充分，还需 durable job、代码/模型/脚手架联合快照、训练接口、版本迁移与采用闭环。Groiz 的部分 canonical 入口仍有未接线/回执路径，本轮未运行 Groiz 验收，不能把已有组件当作完整持续进化能力。

## 1. 评审结论

Zio 已经有语言生态的骨架：Rust 引导、Zio reader/evaluator/compiler、字节码 VM、分阶段自举、运行时镜像、应用打包、宿主边界、格式化、语言服务和库。后续应围绕一个问题收敛：**同一个项目，如何在不同执行入口和目标上保持可预测的语义，并完成编写、测试、依赖安装、构建和发布？**

保留“语义独立的通用 Lisp、同像性、交互式开发、核心机制加 Zio 库”的方向。脚本、CLI、嵌入仍是生态闭环的样板；根据用户补充，Agent 查询、编译试验和恢复执行也作为首批用户旅程，不再仅安排成基础完成后的消费者。AI 能力复用语言机制，语言本身保持通用。

最大缺口是语义决策收敛、长会话运行时、项目与包管理、完整发行以及工具间的一致分析。继续增加语法、引擎和高级库，会放大这些基础缺口。机器码 AOT、JIT、完整 MOP 可以在有明确需求和测量后进入主线。

## 2. 当前资产与实际边界

| 领域 | 当前可核对资产 | 尚不能据此推出的结论 |
| --- | --- | --- |
| 语言前端 | [语言参考](language-reference.md)、Rust reader、`SyntaxNode`、Zio reader/expander；[三执行面合同](../langs/core/tests/toolchain_contract.rs) | reader、宏、LSP、Tree-sitter 已有完整统一语义 |
| 编译器 | [Zio compiler](../langs/core/assets/compiler/compiler.zio)、[analyzer](../langs/core/assets/compiler/analyze.zio)、[自举合同](../langs/core/tests/self_hosting.rs)；L02 记录 Stage 1/2/3 规范制品一致 | 整个运行时已用 Zio 实现、所有宏阶段完全等价、已有机器码后端或性能优势 |
| 执行器 | [AST evaluator](../langs/core/src/eval.rs)、[元循环入口](../langs/core/src/interpreter.rs)、[字节码 VM](../langs/core/src/bytecode.rs) | 切到 VM 自动获得线程安全、GC 或并行能力 |
| 镜像 | [save/restore/wire](../langs/core/src/image/mod.rs)、[镜像合同](../langs/core/tests/image_contract.rs)；M00–M02 有历史验收记录 | 任意宿主资源、活动调用栈、JS heap 都能序列化；恢复等于恢复权限 |
| 应用制品 | [共享 bundle](../langs/core/src/bundle.rs)、[CLI builder](../langs/cli/src/bundle.rs)、[制品合同](../langs/cli/tests/bundle_contract.rs)；A00–A02 有历史证据 | native 是机器码 AOT；浏览器包已离线自包含；WASI 构建不需要开发工具 |
| 互操作 | [原生 QuickJS](../contribs/native/js/src/lib.rs)、[JS 库](../libs/interop/js.zio)、浏览器 bridge；J00 有验收记录 | 原生/浏览器 × AST/compiled 四组合一致；Rust engine、inline、raw block 已完成 |
| 开发工具 | [formatter](../langs/core/src/fmt.rs)、[LSP](../contribs/language-server/src/main.rs)、[静态分析](../contribs/language-server/src/analysis.rs)、[Tree-sitter](../contribs/tree-sitter-zio/grammar.js)、编辑器插件 | LSP 已复用编译器全部名称/宏解析；已有 debugger、profiler 和项目系统 |
| 库与发行 | `libs/std`、Rill、Numa、Loom、learning；[安装器](../tools/install.sh)、[发布流水线](../.github/workflows/release.yml) | 库已独立版本化、包管理器已交付、所有平台均已运行验收、完整开发发行已完成 |

状态细节见[旧工具链计划的工作包证据](superpowers/plans/2026-10-11-zio-language-toolchain.md)。A01 明确记录 Node 实际 WASM 路径与真实浏览器验收的区别，不能把前者升级为 DOM/浏览器完整验收。A02 当前把每个 payload 编进 WASI runtime，涉及逐应用 Rust 构建；这与最终发行要求的“用户构建 Zio 应用不需要 Rust”仍有距离。

## 3. 应保留的设计

1. **语言语义独立于宿主。** Rust 承担值、执行机制和可信能力，Zio 表达前端与库。自举是可维护性与可扩展性的证据，性能须另测。
2. **以可观察合同约束多实现。** fresh context 比较值类别、错误类别/载荷、来源位置和副作用，比只比较打印结果可靠。继续维护显式 VM 验证，避免 AST 回退掩盖差异。
3. **静态制品与动态能力共同存在。** 脚本、编译模块、镜像和应用有不同用途；保留 eval/动态加载时，依赖和能力必须进入 manifest。
4. **授权由宿主决定。** 模块加载、I/O、引擎、镜像恢复都应通过显式授权；包声明需求不能自行取得权限。
5. **四层职责清楚。** `langs/`、`libs/`、`contribs/`、`apps/` 保留。ZOS 是已采纳的核心方向；应明确最小对象机制与可选扩展，而不是在本次评审中擅自移走对象系统。

## 4. 优先解决的设计问题

### 4.1 名称与控制流的意外行为

[语言参考](language-reference.md)已诚实记录以下现状，但“实现有合同”与“适合长期冻结”是两件事：

| 现状 | 长期代价 | 建议方向（待采纳） |
| --- | --- | --- |
| `let` 与 `let*` 均顺序绑定，`loop` initializer 在外层求值 | 相似外观却有不同可见范围 | 保留顺序 `let`，将 `let*` 明确为兼容别名；单独规范循环初始化 |
| `set!` 找不到绑定时创建当前绑定 | 拼写错误变成新状态，静态工具难发现 | 新语言版本中拒绝未声明赋值，`def` 负责创建 |
| `require :as` 仍默认导入全部 exports；alias 是 map | 名称碰撞、示例容易误用 `alias/name` | 新版本显式 `:refer`，真正 namespace alias 单独提案 |
| 多个匹配 catch 由源码最后一个优先；没有 finally | 阅读次序与行为不一致，资源清理无法表达为统一合同 | 结构化错误、首个匹配、明确清理机制及错误覆盖规则 |
| `and/or` 特殊形式与同名函数语义不同 | 间接调用会丢失短路及部分返回值语义 | 规范为语法操作；同名函数兼容期提示，提供名称清楚的严格函数 |

这些变更都可能破坏既有程序，必须有版本边界、旧语义语料、lint/迁移说明，再同步 AST、元循环、compiler、LSP 和文档，不能作为零散“修 bug”静默改变。

### 4.2 同像性需要补齐阶段与绑定模型

现有过程宏接收 datum；`syntax-rules` 只覆盖部分卫生行为，自由标识符仍在调用点按名解析。AST 遇调用时展开，编译入口在编译阶段展开，依赖运行时局部捕获的宏不能直接搬到编译期。这些是语言设计边界。

建议：保留 datum 宏作为显式高级入口；逐步建立有词法绑定身份和来源链的 syntax object，定义 definition-site/use-site、展开阶段导入、编译缓存输入、宏执行能力。编辑器不因打开文件就执行任意宏。先做跨模块宏的最小闭环，再讨论更复杂 reader 扩展与外语 raw block。

README 的“没有特殊形式不可用宏替代”比[哲学文档](zio-philosophy.md)当前论述更强；后者已经正确说明宏依赖最小求值、绑定和控制原语，应统一措辞。

### 4.3 数值、等价、集合和对象身份需一起决定

[Value 的相等与哈希](../langs/core/src/value.rs)按值类别区分整数/浮点，浮点使用 bits；buffer/function/channel 等有身份语义。集合字面量展开成 map-backed `set`。这些选择会影响字典键、集合、镜像去重、FFI 和未来优化。

应建立一张规范矩阵：`nil`/空列表、`1`/`1.0`、正负零、NaN、list/vector、对象、buffer、循环图分别如何比较、排序、打印和作为 key。不要为了让 `1 = 1.0` 看起来方便而破坏 equal/hash 合同。数值 v1 可以维持 i64/f64 和截断除法，但需统一溢出、转换和非有限数规则。独立 Set 类型与 BigInt 都应由需求决定是否进入第一稳定版。

ZOS 还需明确 slot 更新是共享身份变异还是复制返回、类重定义的旧实例行为、方法缓存失效、protocol 与 generic function 的关系。当前 accessor/initarg 元数据不等于完整 accessor/initarg 行为。

### 4.4 VM 出现后，应重新打开 GC 与并发决策

[ADR-014/015](adrs.md)把并发和内存方案推迟到 VM 阶段；现在 VM 已存在。[Closure](../langs/core/src/bytecode.rs)仍捕获 `Rc<RefCell<BindingCell>>`，函数保留 `Arc<Env>`，所以“已有 VM”不能解除 !Send 与环引用问题。纯 `Weak<Env>` 会让逃逸闭包失效，不能直接作为等价 GC 替代。

建议先做 VM/宿主/FFI roots 清单和可追踪堆原型，验证闭包、ZOS、宏、镜像及宿主句柄共同存活与释放。并发第一步采用隔离 VM 与可传递消息子集，定义复制/编码、错误、取消、任务退出和资源关闭；不要直接发送任意 Value，也不先把所有 Env 加锁。该建议会细化 ADR-014，须另行评审，不能当作已采纳新模型。

另有一处实现与原则的张力：README 声称无 thread-local 全局可变状态，而 `bytecode.rs` 已有编译来源/绑定登记的 thread-local 状态。应明确它们是可证明隔离的机制缓存，还是应迁入 context；验证同线程多 context、重入、异常清理和释放。不把这项发现直接判为行为 bug。

### 4.5 生态缺少项目和依赖的统一模型

现有 `require` 解决模块加载，不能替代包管理。模块、发布包、源码项目、锁定依赖图、目标 profile 和制品应是不同概念。建议先支持 path/git 包源及不可变内容摘要、锁文件、离线缓存和确定性解析，再建设公共 registry。

包版本、语言版本、bytecode/image 格式、标准库 API、宿主 ABI 需分开协商。`zio test/check/build/doc` 应读取同一项目模型。源码、宏输入、引擎、目标、编译器和授权配置进入构建缓存键；禁止以本地 PATH、绝对路径或未声明网络访问作为隐藏输入。

### 4.6 工具应消费统一语义信息

LSP 当前具有独立的 token/tree、scope、binding 和 import 分析代码。它不必执行编译器才能工作，但名字解析、模块导出、诊断类别和 source provenance 应有共享合同。否则语言每次改名或加语法都需要多处手工同步。

建议建立无副作用分析接口，供 compiler、LSP、lint、doc、debugger 使用；格式化继续保留注释与原 token。先交付补全/跳转/引用/诊断和 REPL 中断/堆栈，再做断点、单步、变量检查、CPU/分配分析。

## 5. 文档与计划中的冲突

| 冲突 | 处置建议 |
| --- | --- |
| README 旧 Phase 估时与新工作包并存 | 旧章节保留历史性质；当前排期只从验收状态生成 |
| 语言参考开头仍称镜像/原生 JS/WASI 为规划，后文已有这些入口 | 按工作包和实际目标逐项更新概述，保留 Experimental 标记 |
| 特性矩阵前言与部分具体行状态不一致 | 前言引用具体行，不重复维护第二套完成结论 |
| README“所有特殊形式可由宏替代”与哲学文档不一致 | 采用哲学文档的最小原语论述 |
| “所有可变状态显式”与 compiler thread-local 实现存在张力 | RFC 说明允许的机制缓存及隔离条件，或迁入显式 context |
| A01 的部分完成勾选强于其真实浏览器证据 | 将引擎验收、浏览器行为验收、离线自包含分别记录 |

本次只新增评审/决策/路线图及入口链接，不批量重写历史状态。建议在路线图 R0 中集中核对，防止用这份评审替代权威能力矩阵。

## 6. 调研范围与限制

GitNexus 已绑定 `zio`，由落后 32 commits 的索引刷新至上述评审基线，完成概念 query、`Closure`/编译入口 context 与文档 impact。刷新结果为 5,346 nodes、15,222 edges、409 flows；图分析报告入口/分支覆盖上限和部分跨语言解析缺口，因此空调用集不视为无影响。

修改范围只有文档和图表。`docs/roadmap.md` upstream 为 MEDIUM：34 个受影响节点，直接引用为 README/架构文档，图中无运行流程归属；README 为 UNKNOWN，已补查文本引用，并保留原链接和章节。新增文档尚无既有图节点，不能给出“零影响”保证。后续涉及语言实现时，必须重新对实际目标符号运行 impact。

本评审不对第三方工具版本、最新行业方案或外部性能做结论。运行时合同、真实浏览器/WASI/平台行为仍须在各工作包中执行验收；文档和图表检查不替代它们。

## 7. 首轮评审验证（AI 补充前的记录）

- 新增三份 Markdown 的 41 个本地链接均可定位；`git diff --check` 通过。
- 站点 `bun run build` 成功生成 85 页；这只验证文档构建，不代表语言能力验收。
- 交互式决策图为 architecture 类型的主题树，连线只表达父子主题归属，端点已完整表达关系，因此省略重复的边标签。图表通过 showcase 的 9 项检查，0 errors、0 warnings；[制品回执](design/zio-design-decisions.receipt.json)记录规范/HTML 的 SHA-256 和字节数。
- 真实 Chrome 在 1440×900、1600×1000、1920×1080、2048×1320 均无页面溢出；已目视检查最小尺寸浅色与最大尺寸深色截图。截图与浏览器检查原始回执保存在本次工作环境的 `/tmp/zio-design-visual-evidence/`，不作为永久仓库依赖。
- 使用临时 Git index 将五个新文档/图表源文件与两个已跟踪入口文件共同纳入 `detect-changes --scope all --limit 10000`；返回 7 files、3 个既有文档 section、0 affected processes、LOW。没有改动实际暂存区。新增文件尚无旧图符号，零流程不证明新内容无影响；新增链接与内容另做人工核对，本次没有语言实现变更。
