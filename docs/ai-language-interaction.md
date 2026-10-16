# Zio 的 AI 交互设计：查询、实验、恢复与 MCP

日期：2026-10-16。状态：**§4 查询合同已定案（ZIO-003）；其余接口与实现方案仍为待评审草案**。

用户要求：Zio 面向 AI 时代，要让 Agent 能理解语言、查询函数、自由编译和探索特性；以 Common Lisp condition/restart 为恢复核心，并可由解释器提供 MCP 服务。本文把这些要求纳入语言与工具链主线，不将其限定为 Loom 或某个模型供应商功能。查询实现及实跑证据以[特性矩阵](feature-matrix.md)与 paca ZIO-004 为准；本文不把后续 harness/restart/MCP 草案写成已实现。

关联：[关键决策 D09/D19–D21](language-design-decisions.md) · [生态路线图](ecosystem-roadmap.md) · [编译器/VM/GC 入门](../blog/05-compiler-vm-gc.md) · [condition/restart 与 AI 博客](../blog/06-restarts-help-harness-mcp.md)。

用户进一步明确持续运行、自行优化代码、训练模型与组装脚手架；对照 Groiz 后的[持续演化补充](continuous-evolution-design.md)新增 D22～D26 与 EVO-01～07。本文解决 AI 如何进入语言运行时；持续演化还需要持久任务、联合版本、训练后端和采用策略，不能仅用 MCP/restart 代表全部能力。

多实例联结、分工和讨论见[群体协作补充](collective-intelligence-design.md)。MCP/会话适配提供访问表面，协作组还需实例身份、委派、消息去重、讨论/决策和跨实例预算合同。其他实例可以提供恢复建议，原 run 的 owner 仍校验 restart；远程句柄不因加入协作组而可随意转移。

## 1. 核心目标与现有基础

Agent 应能完成这样的闭环：查询当前语言能力 → 找到函数与示例 → 在会话中编译/运行小程序 → 检查实际结果与副作用 → 遇到 condition 时查询可用 restart → 选择恢复动作 → 继续执行 → 得到可复查的实验记录。

理解能力由函数元数据、结构化诊断和实验反馈共同提供。同像性利于代码编辑，但不保证模型正确；restart 为可恢复问题提供协议，但不能自动修复任意 bug。语言仍对人类开发者提供相同查询、交互与恢复能力。

| 能力 | 当前依据 | 需要新增的合同 |
| --- | --- | --- |
| 运行/编译 | AST、元循环、bytecode、self-build | 明确调用面、会话身份、版本、实验记录及失败状态 |
| 文档 | [`;; doc:` 约定](libdoc-convention.md)、静态库参考生成 | 按符号身份关联文档/签名/来源与目标能力的查询索引 |
| 观察/预算 | `EvalRuntime::spend`、`Observation`、host 执行机制 | 限额统一、输出捕获、执行状态与取消/暂停生命周期 |
| 条件/重启 | [ZOS 规格](zos-spec.md)有 condition/restart 方向；当前 try/catch 合同以字符串错误为主 | 未退栈 handler、动态 restart、参数检查、控制转移、清理规则 |
| 现有 Loom harness | 模型调用、会话、工具、预算、取消与回放合同 | **语言执行 harness**；不把 provider 对话 harness 当作可恢复 VM |
| MCP | 本轮检索未发现解释器 MCP server 实现 | 传输适配、工具/资源协议、显式会话与执行句柄、客户端验收 |

## 2. 方案选择

| 路径 | 优点 | 主要代价 |
| --- | --- | --- |
| A. MCP 包装一个无状态 eval | 可较快验证客户端连通 | 缺函数发现、持续会话和恢复；重新执行容易重复副作用 |
| B. 查询模型 + 执行 harness + condition/restart + MCP 适配（推荐） | CLI、REPL、Agent、编辑器共享能力；可分阶段验收 | 需要先定义控制状态、身份、资源与恢复边界 |
| C. 全功能镜像 IDE、热补丁和任意续体 | 交互能力广 | 超出当前镜像合同；堆、FFI、代码版本与安全点复杂度高 |

采用 B 作为建议方案。A 可作为 B 的早期 transport 子集，但只有 eval 不能被称为完整 AI 交互；C 作为独立研究，不阻塞首个可用闭环。

## 3. 一套语言服务，多种入口

```text
CLI / REPL / 编辑器 / 外部 Agent（MCP client）
                  ↓
入口适配：参数、协议、输出呈现
                  ↓
语言查询服务 + 执行 harness
                  ↓
同一 bootstrap、compiler、runtime、模块、condition/restart
                  ↓
IoHost / 引擎 / 隔离 worker / 宿主授权
```

候选 CLI 入口是 `zio mcp --transport stdio`，**尚未实现**。它启动解释器服务模式，调用同一语言装配与编译入口；不为 MCP 另造语法、求值规则或隐藏 AST fallback。协议编解码属于 adapter，动态 condition/restart 属于语言/运行时，模型调用属于可选 Loom。使用外部 Agent 不要求 Zio 内置 LLM。

AST/元循环/compiled 能力应分别声明。首个 MCP 原型可提供已验证入口的 compile/run；需要远程恢复的执行面在可暂停后再公布该能力，不能宣称所有入口都已支持 resume。

## 4. 查询合同 v1（ZIO-003）

选择**实时绑定查询 + 原始源码注释 + native 注册元数据**。不另建可过期的全局
文档数据库，不通过执行目标、宏展开、加载模块、调用 getter/printer 或运行示例
来推断信息。当前类型、文档或效果没有记录时返回未知，不生成猜测。

### 4.1 入口与返回

| 调用 | 返回合同 |
| --- | --- |
| `(describe-symbol 'identity)` | 一个描述 map；符号或字符串指定当前根环境绑定，qualified 名只查已加载模块的显式导出 |
| `(describe-symbol :some.module)` | 已加载模块描述；keyword 明确选择模块命名空间，不与同名绑定混淆 |
| `(help 'identity)` | 从同一个描述渲染人可读文本，经当前 IoHost 输出，返回 nil；脚本与 REPL 相同，不调用目标值的 printer |
| `(apropos "sequence")`、`(apropos "sequence" offset limit)` | 按名称/已有文档不区分 ASCII 大小写检索；返回 `:items` 描述向量、`:offset`、`:next-offset`、`:schema-version`、`:runtime-version` |
| `(module-exports :some.module)` | 模块描述加按名字排序的 `:exports` 描述向量；只列模块自己拥有且显式导出的绑定，不隐式加载 |
| `(language-capabilities)` | 当前 runtime/目标与实际绑定支持的能力 map；未声明的宿主授权为未知，不把符号存在当作 I/O 授权 |

`apropos` 默认 offset=0、limit=20，limit 范围 1..100；非整数、负 offset、
越界 limit 和错误参数类型返回普通参数错误。结果按名字排序、去重；同一环境不变时
可按 offset 分页。定义发生变化后从 offset=0 重新查询；它不是跨请求快照。
搜索范围是当前根环境可见绑定与特殊形式，不扫描文件系统或自动枚举未加载模块。
模块自身的发现使用已知模块名和 `module-exports`。

### 4.2 描述结构与未知值

所有描述共有 `:schema-version 1`、`:runtime-version`（构建版本）、`:status`、
`:id`、`:name`、`:module`、`:module-version`、`:kind`、`:origin`、
`:params`、`:rest`、`:doc`、`:documentation`、`:source`、`:examples`、
`:effects`、`:requires`。模块另有 `:exports`。

- `:status` 为 `:found`、`:not-found` 或模块的 `:not-loaded`；正常查无结果不是运行错误。
- `:kind` 区分 `:function`、`:macro`、`:native`、`:special-form`、`:module`、
  `:class`、`:generic-function`、普通 `:value`；未知种类用 `:unknown`。
- `:params` 是已知固定必需参数的字符串向量；`:rest` 是 rest 参数名或 nil。
  无参数为 `[]`，签名未知为 nil，不能把未知误写成零参数。
  native 可选参数的数量/约束在注册文档中明确，不把可选参数伪装成任意多参支持。
- `:documentation` 为 `:available`、`:missing` 或 `:unknown`；`:doc` 无内容时为 nil。
  原生注册未提供文档/签名时显式未知；源定义没有关联注释时为 missing。
- `:source` 为 nil 或 `{:name ... :revision ... :start ... :end ... :line ... :column ...}`。
  revision 是当前 SourceMap 的源码身份，start/end 是 UTF-8 字节范围，行列从1开始。
  缺来源的生成代码/旧制品保持未知，不按同名文件的最新内容补造来源。
- `:examples` 始终为向量；只提取已有文档中的缩进示例，元素含 `:text` 与 `:source`，
  不执行。它们与描述共享 runtime 版本和原始 source revision；无示例为 `[]`。
- `:id` 是当前会话内的逻辑绑定/模块定位符，不是持久执行句柄；重新定义后重新 describe。
  它不承诺跨会话唯一性、热替换版本绑定或代码采用策略。
- 独立模块版本、效果和能力约束当前没有可信声明则为 nil；nil 表示未知，不表示无副作用、
  无权限要求或许可执行。metadata 本身不是授权证明。

### 4.3 来源与实现边界

固定参数/rest 从当前 callable 取得，不靠文档猜测。源码文档沿用
[`;; doc:` 约定](libdoc-convention.md)：仅关联同一已注册源码版本中、紧邻定义的连续
注释，空行断开；横幅不是符号文档。来源必须能关联到当前 callable 的真实 span，
不能在重定义或 alias 后借用同名旧文档。无法可靠关联的来源/文档返回未知。
native 元数据随 `NativeFn` 身份在注册点提供；未标注 native 不借用同名函数资料。
特殊形式描述遵循求值器优先级（例如 `and`/`or` 不冒充同名 stdlib 普通函数）。

查询只读取 runtime/Env/SourceMap/已加载 ModuleRegistry 和注册元数据；
不新增语言特殊形式、不改变 AST/元循环/显式 VM 的求值规则或 portable wire 版本。
三执行面通过相同 native 查询入口消费同一结构。首版只查根环境；不能通过查询表达式
再次执行用户代码来捕获局部作用域。查询结果不含待查询值本身或其自定义展示。

能力结果至少明确 schema/runtime 版本、编译目标、结构化查询是否可用及
`remote-restart: false`；未实现的长期会话/暂停不能因存在静态镜像而标为可用。
MCP、LSP 接入这些描述属于对应后续任务，不在查询任务中伪造传输服务。

### 4.4 验收与文件范围

ZIO-004 复用 `langs/core/src/{value.rs,env.rs,span.rs,module.rs}` 的现有机制，
新增只读查询实现和源码/native metadata helper，经 `builtins::setup_env` 注册；
help 沿用现有 IoHost 和 REPL 展示，不建立专用 REPL 分支。
不扩展求值器、GC、模块加载或编译策略。
采用现有 Rust 合同测试与实际 CLI/REPL smoke：覆盖函数、宏、native、特殊形式、
模块私有绑定隔离、缺文档/缺符号、签名/rest、源码重定义与 Unicode、确定性分页、
查询/示例/模块查找无副作用，以及 AST/元循环/显式 VM 对照。
schema 或本文约定若必须调整，先同步合同与 paca 证据，不能以缩小测试代替验收。

## 5. condition/restart 是早期核心

condition 表达“发生了什么”，handler 决定“怎样应对”，restart 声明“当前位置允许怎样恢复”。普通 condition 不一定是致命错误，signal 可以被观察后继续；不可继续的 error 若无人处理则进入宿主 debugger/harness 的显式策略。借鉴 [Common Lisp handler-bind](https://www.lispworks.com/documentation/HyperSpec/Body/m_handle.htm) 的动态处理模型；Zio 不宣称 ANSI Common Lisp 完整兼容。

最小机制应覆盖结构化 condition、动态 handler、restart 建立/查询/调用和必要的清理。restart 不放到完整 MOP 之后。完整类层次、更多标准 restart、交互 debugger 和其他扩展可以后续完善。

### 5.1 必须保留的语义

- signal 时先在仍有效的动态环境调用适用 handler；**不能先把错误转换成已退栈 Result，再伪造原现场 restarts**。
- handler 可以拒绝处理，使查找继续；动态嵌套、同层匹配、递归发 signal 和未处理策略均需明确。当前 try/catch 的兼容路径另行保留或版本迁移。
- restart 是动态范围内的实例。同名 restart 可嵌套，名称用于人类展示；外部选择使用实例 ID。查询只暴露对当前 condition 适用且获准的恢复选项。[CL compute-restarts](https://www.lispworks.com/documentation/HyperSpec/Body/f_comp_1.htm)可作为概念参考。
- 区分 restart 的控制模式：`restart-case` 风格会转移到目标子句并退去中间动态帧；返回式 handler/restart 若支持则单独定义。**有些恢复会退部分栈，但不是先退完整个失败调用再恢复。**[CL restart-case](https://www.lispworks.com/documentation/HyperSpec/Body/m_rst_ca.htm)明确规定其控制转移和退栈。
- `retry` 只有在库显式提供并定义重试范围时才存在；`use-value`、跳过项等也由恢复点规定，运行时不为每个错误自动创造“安全恢复”。调用后原恢复点离开动态范围，旧句柄失效。
- 清理规则覆盖正常返回、转移到 restart、abort、取消及清理自身失败；等待外部选择期间，保留帧中的资源仍活着，不提前清理。

### 5.2 面向 Agent 的恢复描述

每项 restart 提供实例 ID、名称、说明、参数 schema/约束、有效动态范围、适用 condition、预期效果和能力需求。condition 提供稳定类别、数据、来源与调用摘要；敏感值由宿主展示策略处理。自然语言 report 供阅读，不能作为唯一机器协议。

例如缺少输入数值，程序可以显式提供 `use-value(integer)` 和 `abort`。Agent 传入整数后继续使用该恢复值；若传类型错误、旧 ID 或没有权限，则不推进计算，不消费其他 restart。文件/网络授权拒绝不能通过 `retry` 自动变成授权成功。

## 6. 语言执行 harness

harness 是执行会话、试验、预算和观察的编排层。它不代替 VM、GC 或 condition 机制，也不负责选择哪个模型“更聪明”。

会话记录语言/runtime 版本、模块根、工作目录、已授予能力、metadata revision 与配额。Agent 在已授予的范围内可以多次编译、运行和查询，不要求每次实验重新授权；需要新增能力时明确返回缺少何种授权，由宿主现有策略决定。

| 动作（候选） | 输入重点 | 输出重点 |
| --- | --- | --- |
| open/close session | target、engine、workspace、预算；授权来自宿主 | 显式 session_id、实际能力；关闭后句柄失效 |
| query | session_id、名字/ID、过滤/分页 | 结构化 metadata、revision、下一页 |
| check/expand/compile | source + source name/revision、阶段权限、依赖 | 诊断、展开来源/制品 ID、依赖与输入摘要 |
| run | source 或制品、engine、预算、恢复策略 | run_id、状态、值/句柄、输出/事件、condition |
| inspect/restarts | session_id、run_id、暂停 epoch、帧/对象范围 | 有界只读预览、适用恢复描述 |
| invoke-restart | 暂停 epoch、restart ID、参数、请求去重键 | 新状态；有效选择才推进，重复请求不重复执行 |
| cancel/release | run/handle ID、期望版本 | 终态与清理结果；不会返回到成功执行态 |

不把第一次运行的全部状态自动发布为“稳定模块”。可提供干净子会话用于试验；是否采用候选定义/模块由明确操作决定。若试验使用镜像复制，只对已支持的静止 portable 子集承诺；有宿主资源的会话不能默认 fork。

编译同样是执行边界：宏有自己的预算、权限和输出记录；`check` 的无副作用分析与显式 macroexpand 不能混为一谈。Zio fuel 限制已接入执行路径，但 native/JS/Rust 阻塞调用还需引擎中断或可终止 worker，以及时间、内存和输出上限。

### 6.1 暂停状态机

```text
idle → compiling/running → completed
                       ↘ failed / cancelled
                       ↘ suspended(condition, epoch)
                            → 有效 invoke-restart → running
                            → abort/cancel/过期 → cancelled/failed（按合同清理）
```

“先返回暂停结果，下一次工具调用再继续”需要实际保留 VM/解释器状态与动态 handler/restart 链。先验收同进程同步 handler 恢复，再实现可暂停执行帧；在后者完成前，MCP 可运行实验但 `remote_restart: false`。

暂停时计算没有退栈。帧、闭包、condition 与 restart 都是 GC roots；对暂停会话只允许有界只读查询、选择恢复或取消，不允许并发 eval 改写其环境。新的探索走独立会话。native callback 若不能安全暂停，执行面必须明确拒绝或在可保留现场的 worker 边界处理，不能伪造续体。

首轮只承诺活着的 worker 中的暂停状态；进程重启后句柄失效。现有静止镜像不保存在途调用栈，因此不能当作远程恢复或 crash-resume 的实现。通用 first-class continuation 也不是最小 condition/restart 的前提。

需要跨进程存活的等待与学习任务走 durable job/checkpoint：持久化阶段、进度、事件、预算与恢复描述，再以新 attempt 接管；不要求把所有等待都保存成任意 VM continuation。任务/worker 的 owner epoch 与这里的暂停 epoch 不同，均需明确身份和失效规则。

### 6.2 副作用与可复查

返回状态须区分 completed、suspended、failed、cancelled，并分别给出 structured value/condition、来源、stdout/stderr、消耗、截断标志。闭包/对象使用带会话与代际的句柄；i64/非有限数等采用有类别标签的 wire value，不能直接经 JSON Number 丢精度。

每轮记录 source revision、编译器/目标/模块版本、初始状态标识、请求 ID、选中的 restart 与结果。随机/时钟/I/O 只有通过可记录 adapter 才能回放；记录实验不等于任意外部世界可确定重演。

取消和异常不自动撤销已经发生的文件写入或网络调用。只有明确的事务/补偿 API 才能承诺 rollback。恢复动作的范围必须比“再跑整个程序”更精确，并能观察是否重复产生副作用。

## 7. 解释器作为 MCP server

首轮建议本地 stdio；网络服务单独验证身份、隔离与路由。候选工具包括 `zio_capabilities`、`zio_session_open/close`、`zio_query`、`zio_compile`、`zio_run`、`zio_inspect`、`zio_restart`、`zio_cancel`。一组稳定工具按 session/run ID 查询动态值与 restart，避免把每个用户函数都注册成新 MCP tool。

长期应用还应能查询受授权的 job、snapshot、checkpoint 与实际能力；这些是通用执行服务及应用 adapter 的数据。训练/采用等 Groiz 领域动作由应用扩展暴露，语言 core 不硬编码。session/run/句柄固定 snapshot/revision；活动版本变化不能让旧句柄悄悄读取新版。详细生命周期见持续演化补充。

MCP 负责工具与资源呈现，运行时会话属于 Zio。设计参考明确版本的 [MCP 2026-07-28 tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools) 与 [basic](https://modelcontextprotocol.io/specification/2026-07-28/basic)：该版协议核心不以连接维持会话，故每次调用显式传 Zio session/run 句柄；支持旧版客户端时通过明确兼容适配，不混用新旧初始化规则。工具结果支持 schema 和结构化数据，但不因此获得执行权限。

stdout 只写 MCP 消息；用户程序的 println 捕获到执行结果，服务日志写 stderr，不能让 REPL 提示符混入协议流。遵循 [stdio 传输约束](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio)。

transport 请求 ID 与语言 run ID 不同。恢复调用含 session_id、run_id、epoch、restart_id、args 和 operation_id；重复 operation_id 返回已记录结果，跨调用者、跨会话、过期或错误 epoch 均拒绝。取消 MCP 请求与取消后台执行分开映射；会话关闭或租约到期必须释放 worker/roots。短暂传输中断不自动恢复或重放执行，续接按显式身份与租约处理。

`suspended` 是 Zio 应用层状态，不伪装成 MCP 自带 continuation。即使采用该版的 multi round-trip 交互，动态执行现场仍由 runtime/harness 持有；首轮不依赖客户端主动支持复杂嵌套回调。

## 8. 分阶段验收

| 阶段 | 交付 | 必须观察的结果 |
| --- | --- | --- |
| AI-01 查询 | 元数据索引、help/describe/apropos/capabilities | 内置/库/宏/特殊形式可区分；读取无执行；缺信息明确未知；CLI/MCP 查询结果一致 |
| AI-02 本地恢复 | 结构化 condition、未退栈 handler、动态 restart 和清理 | 嵌套同名恢复点正确选择、参数检查、handler 拒绝、控制转移；前置输出不因恢复重复 |
| AI-03 实验 harness | 显式会话、compile/run、预算、输出、事件与取消 | fresh/持续会话区别清楚；编译期权限和预算生效；失败后可继续新实验；结果保留类型与来源 |
| AI-04 MCP 基础 | stdio adapter 与客户端兼容表 | 真客户端完成 discover/query/compile/run/close；并行请求不串会话；stdout 无协议污染；未完成远程恢复如实声明 |
| AI-05 远程恢复 | VM 暂停帧、动态链、GC roots、restart/epoch/去重与租约 | 首次运行暂停后释放当前工具调用，后续选择在同一现场继续；重复/过期/跨会话拒绝；取消和掉线清理可观察 |
| AI-06 语言探索基准 | 固定任务集与实验记录 | Agent 从查询文档到运行、选择恢复和交付结果闭环；按成功率、无效调用、token/时间、恢复副作用计分 |

AI-02 与 AI-03 可在接口固定后分别推进；AI-04 先接已有安全执行面，不等 AI-05 才能验证 MCP 价值。AI-05 依赖 R2 的 roots/控制帧和清理设计，但不要求所有目标、所有 native 句柄都可暂停。跨执行面不支持项必须显式拒绝。

探索基准至少包括陌生函数发现、宏展开定位、编译/解释已知答案比较、缺失值的 use-value、可控 retry 不重复前置写入、无可用恢复点时 abort、暂停时 GC、超时取消与 MCP 重连后的过期句柄。语言已有测试不等于 Agent 自动取得这些能力，AI 竞争力以这条完整闭环衡量。
