# ZIO execution harness contract

- 状态：D20 — 已采纳（设计文档）。实现 = ZIO-008；MCP 绑定 = ZIO-009。
- 上游设计：`docs/superpowers/specs/2026-10-11-zio-language-toolchain-design.md`（§4 语义、§5 制品、§6 镜像、§7 外部引擎、§8 分发、§9 CLI、§10 验证矩阵）。
- 验收对应：ZIO-007（本文件）、ZIO-008（实现，P0）、ZIO-009（MCP，P0，前置 ZIO-008）。

## 1. Status & scope

本文件是 Zio 执行 harness 的合同（D20，ZIO-007）：定义会话（session）、编译/执行/运行（compile/eval/run）的结果形状、输出捕获与上限、预算（budget）、取消（cancel）、错误分类，以及 reader/宏展开/编译/VM/native 各阶段的限制和宿主调用（host call）的能力边界。harness 是库代码，不负责启动（launcher = CLI / MCP 服务 / 镜像恢复宿主 / inline 引擎宿主）。行为由 ZIO-008 实现；本文只做合同，可评审、可按 §15 表格逐项验收。术语沿用 `docs/glossary.md`（reader、AST、宏展开、bytecode、VM、native、镜像、observation、fuel、restart、module、artifact）；与 `docs/feature-matrix.md` 状态基线一致，不超卖能力：会话不存活 VM、静态镜像不等于活动执行栈（见 §14）。

## 2. Session lifecycle

- `SessionId`：uuid-style（v4 随机）。由 launcher 通过 harness 的 `Session::new(capabilities, defaults)` 创建，harness 不自造。
- 标识稳定性：同一 `SessionId` 在多次 run 之间稳定；会话持有尽量少的状态：已编译模块注册表、能力集（§4，不可变）、预算默认值、poison 标记（§9）。
- 新 run = 新执行环境：每次 `run` 在全新 `language_context` / VM 上下文里执行（见 §3）。会话不缓存 VM、不跨 run 透传用户可见状态 —— 与"静态镜像 ≠ 保存活动执行栈"（任务清单 §2）一致。
- 会话结束：显式 `close_session`（释放模块、宿主句柄）；进程退出隐式结束；空闲超时是 launcher 策略，非 harness 合同。
- 传输无关：MCP 场景的会话状态在服务端，以 `SessionId` 为键；新连接携带同一 `SessionId` 即恢复同一会话。会话绝不依赖传输连接身份（ZIO-009 验收"会话不依赖传输连接身份"）。

## 3. Run identity

- `RunId`：uuid-style（v4 随机）。每次 `compile` / `eval` / `run` 调用产生一个，并在结果信封中原样返回（§5/§6）。
- 关系：一个 `SessionId` 下可有多个 `RunId`（many-to-one）；run 之间相互独立、无共享执行状态。
- 生命周期：`RunId` 在结果投递后即失效，不可复用；`cancel` 按 `SessionId + RunId` 寻址（§9）。
- 合同：`RunId` 由 harness 生成而非 launcher，避免传输层伪造/碰撞（MCP 往返验收见 §13）。

## 4. Capability negotiation

- 现状：宿主原语按注册挂载（`contribs/native/loom/src/lib.rs` 的 `install` 路径：native 函数注册时声明能力；无宿主时调用得到 `capability-denied:`；见 `loom/src/lib.rs:198, 316`）；`langs/cli/src/commands.rs` 已有 `--lib-dir` / 模块根目录授予模型。镜像绑定根是具名根（M02 的 root-names 合同，`langs/cli/src/commands.rs`）。
- 合同：harness 自身**不授予**任何能力。能力由 launcher 在创建会话时注入：CLI（`--lib-dir`、模块根、`--allow-*` 开关）、MCP（服务配置）、镜像恢复宿主（镜像内声明的根，恢复时重新授予）、inline 引擎宿主（embedder 显式传入）。预期的能力位：文件 I/O 根、网络、进程派生、JS 引擎访问（与 `contribs/native/js` J00 合同对齐）。
- 能力集在会话创建时确定，会话存活期内不可变；需要变更 → 新建会话。
- 被拒能力 → 类型化错误 `CapabilityDenied`（§10），不是通用失败。

## 5. Compile result

- 信封：`{ module, exports, dependencies, warnings, errors[], run_id }`。
  - `module`：`bytecode::Module` 句柄（经 `bytecode::compile_source`（`langs/core/src/bytecode.rs:575`）产出；`langs/core/src/bytecode.rs`），或注册到会话的具名模块句柄；持久化用 `serialize_module`（`langs/core/src/bytecode.rs:619`，对齐上游设计 §5 artifacts）。
  - `exports` / `dependencies`：模块导出符号表与依赖模块名（对齐 `docs/glossary.md` 的 module 术语；供 MCP 查询复用，配合 ZIO-003/004 的 `NativeMetadata` 结构）。
  - `warnings`：诊断列表。
  - `errors[]`：每条携带 `span`（源位置：偏移/行/列；复用编译器现有 span 类型）。
  - `run_id`：本次调用的 `RunId`（§3）。
- 合同：编译是**静态阶段**，不执行任何用户代码（对齐 ZIO-003"查询不执行被查询代码"与上游设计 §4 的静态语义）。`errors[]` 非空时 `module` 为缺省句柄或空，依 ZIO-008 对错误聚合/首错的选择（开放问题 §16）。

## 6. Run result

- 信封：`{ value | error, output[], events[], fuel_spent, run_id }`。
- `value`：内部值表示（既有 VM 值类型），**保留大整数等值精度，不做 JSON 往返**；仅在显式转换/表示时产十进制字符串（ZIO-008 验收"保留大整数等值的精度，不依赖字符串解析错误"）。MCP 传输精度语义在 ZIO-009 定（开放问题）。
- `output[]`：被捕获的 stdout 风格打印串（§7）；`events[]`：带 `span` 的 observer 事件（`langs/core/src/observer.rs` 的 `Observer` 事件流；`Observation::spend_fuel` 记录点 `langs/core/src/observer.rs:203`）。
- `fuel_spent`：本次 run 消耗的 fuel（§8）。
- 二选一：`value` 或 `error`；两者并存时以 `error` 为准（错误即结果，见 §10）。

## 7. Output capture

- 三条流：stdout、stderr、诊断事件（diagnostic events）。诊断事件带 `span`（对齐 §6 events）；stdout/stderr 是已渲染文本。
- 每条流有界：`max_output_bytes` 上限（默认值开放问题 §16）。溢出策略：**截断 + 发出 `OutputTruncated` 警告事件**，不使 run 失败 —— 失控打印应由预算/取消终止（§8/§9），而不是输出上限误杀。截断与预算耗尽是两个可区分的结果条件。
- 合同：若 launcher 的传输拥有真实 stdout（如 MCP stdio），harness **绝不**向真实 stdout 写任何内容（§13；协议流独占）。

## 8. Budget

- 预算项：`fuel`（类指令计数）、`max_duration`（墙钟）、`max_memory`（堆上限）、`max_output_bytes`（§7）。每项可选用（opt-in），未给时用默认值。
- `fuel` 映射既有 `Observation::spend_fuel`（`langs/core/src/observer.rs:203`）；耗尽 → `BudgetExhausted`（§10）。既有 `EvalError::step_limit_exhausted`（`langs/core/src/eval.rs:136`）是同类事件的现成模式，fuel 化时复用其路径而不再另立机制。
- 阶段归属（与 §11 表一致）：

| 预算项 | reader | 宏展开 | 编译 | VM | native |
|---|---|---|---|---|---|
| fuel | — | ✓（宏是用户代码） | — | ✓ | ✓（宿主自觉扣减） |
| max_duration | ✓ | ✓ | ✓ | ✓ | ✓ |
| max_memory | — | ✓ | — | ✓ | ✓ |
| max_output_bytes | — | — | — | ✓ | ✓ |

## 9. Cancellation

- 可取消：`eval`（在 `Observer` 检查点协作检查）、VM 执行（fuel 检查点，复用 §8 `spend_fuel` 路径）、native 调用（宿主支持协作中断时，如 `contribs/native/js` 的 `install_interrupt` `contribs/native/js/src/lib.rs:161`）。
- 不可取消（现状）：Rust 编译器进程（`compile`）、QuickJS 解释执行（尚未接中断；J00 现状为跑完为止）。
- 非可取消调用的隔离策略：超时 → **kill 进程组**（宿主调用放入独立子进程/worker 以确保可杀）→ 会话标记 **poisoned**（进程组被杀后 in-memory 不变量不可信，会话不可再复用）→ 向调用方返回干净的 `Cancelled`（带 `isolation: process-group-killed` 注记，字段命名开放）。poison 是会话级标记；新会话不受影响。
- `cancel` 寻址：`SessionId + RunId`；仅对进行中的 run 有效；已投递结果的 `RunId` 不可取消（§3）。

## 10. Errors

| 类型 | 来源 | span | 说明 |
|---|---|---|---|
| `EvalError` | 既有（`langs/core/src/eval.rs`） | ✓ | 含既有 `step_limit_exhausted` 模式（`langs/core/src/eval.rs:136`） |
| `CompileError` | 编译阶段（proposed 类型） | ✓ | §5 errors[] |
| `LoadError` | `image::restore` / 模块加载 | 有则带 | §14 |
| `CapabilityDenied` | 宿主调用能力检查 | 有则带 | 对齐 loom `capability-denied:`（`loom/src/lib.rs:198`） |
| `BudgetExhausted` | fuel/内存/输出上限 | 有则带 | §8 |
| `Cancelled` | 取消/隔离清理 | 有则带 | §9 |

- 合同：**错误不改变会话**；非致命错误后会话保持可用（ZIO-008 验收"失败后会话行为明确"：下一条 `compile`/`run` 照常）。唯一例外是 poison 标记（§9）——poison 状态下只有 `close_session` 合法。
- 每个错误尽量携带 `span`；`EvalError` 既有 span 语义直接沿用，不复制一套错误体系。

## 11. Stage limits（阶段限制总表）

| 阶段 | 入口（现状） | 可取消 | 适用预算 | 隔离 |
|---|---|---|---|---|
| reader | 源 → AST（`langs/core/src/reader`，经 `compile_source` 前段） | 是（表单元间检查） | max_duration | 进程内 |
| 宏展开 | 宏阶段（AST → AST） | 是（检查点） | fuel + max_duration + max_memory | 进程内 |
| 编译 | `bytecode::compile_source`（`langs/core/src/bytecode.rs:575`） | **否**（Rust 编译进程） | max_duration | 进程内；超时整体报错，不 poison |
| VM | `EvalError`/`language_context` 驱动（`langs/core/src/eval.rs:97, 109, 115`，`langs/core/src/bootstrap.rs:251`） | 是（fuel/观察点） | fuel + max_duration + max_memory | 进程内 |
| native | 宿主注册调用 | 协作（宿主支持时） | max_duration + max_memory | 宿主决定；需要 kill 时子进程/worker（§9） |

对齐上游设计 §7（外部引擎）的边界：引擎自身的执行约束由引擎宿主负责，harness 只负责按上表施加预算与取消传递。

## 12. Host-call limits（宿主调用限制）

- 用户代码可调用的宿主原语 = **已注册的 native 函数**，且在注册时声明能力集（`contribs/native/loom/src/lib.rs` 的 `install` 路径）。无宿主可挂载 → 调用返回 `capability-denied:`（loom 现状行为 `loom/src/lib.rs:166, 198`；harness 层映射为类型化 `CapabilityDenied`，§10）。
- 需要能力才可调用：文件 I/O 根、网络、进程派生、JS 引擎访问等（§4 能力位）。CLI 侧 `--lib-dir` / 模块根授予即是对"模块内宿主调用"的既有门禁（`langs/cli/src/commands.rs`），harness 沿用同样的"根内可访问、根外拒绝"模型。
- 非可取消宿主调用的隔离：`contribs/native/js` 的 `install_interrupt`（`contribs/native/js/src/lib.rs:161`）与 `set_memory_limit`（`contribs/native/js/src/lib.rs:203`）作为协作式参考；宿主不支持协作中断时落到 §9 的 timeout → kill 进程组 → poison → 干净 `Cancelled`。
- native 调用在调用边界再次校验能力并记录 fuel（宿主自觉扣减，§8）；校验结果对调用方不可见为"能力存在性探测"——能力查询属于 ZIO-003/004 的元数据合同，不在此扩展。

## 13. MCP binding（ZIO-009 前瞻合同）

- 方法集合：`create_session`、`compile`、`eval`、`run`、`cancel`、`close_session`（命名由 ZIO-009 定案；当前为前缀建议）。
- 会话状态在服务端（`SessionId` 键控），不在传输层；传输连接断开只影响该连接的请求，不销毁会话（§2）。
- 结果信封与 §5/§6 一致；`run_id` 必须在响应中往返并等于请求上下文中的 `RunId`（验收：MCP 往返保留 RunId）。
- **stdout 仅供协议流**：`output[]` 以 JSON 字段传输，harness/服务绝不写真实 stdout（§7）。
- 复用同一 harness（ZIO-009 验收"复用同一 harness"）；MCP 层只做信封编解码与会话注册，不重实现执行逻辑。
- 首版明确不支持远程 restart（condition/restart 恢复是 ZIO-012 范畴；任务清单"长期运行的'恢复'分三种"），MCP 文档如实声明。

## 14. Image interaction

- harness 经 `image::restore_image`（`langs/core/src/image/restore.rs:31`）载入 `.zio-image`（`langs/core` 镜像模块，M02 已提供 save/restore 与 CLI 合同：`zio image save/inspect/run`，`langs/cli/src/commands.rs`），在**全新上下文**中运行，并按名绑定镜像根（root-names 合同）。
- 恢复不绕过授予边界：镜像本身不携带能力，恢复后的能力仍由 launcher 按 §4 授予；镜像内任一根外访问 → `CapabilityDenied`。
- 静态镜像 ≠ 保存的活动执行栈（任务清单 §2 与 ZIO-012 明确区分）；harness v1 不做恢复后暂停/续跑，镜像载入只提供初始模块与环境。

## 15. Verification matrix

| # | 检查项 | 期望 | 阶段 |
|---|---|---|---|
| 1 | 取消中断 fuel 有界 eval | 运行中 `cancel` → 结果收 `Cancelled`，`fuel_spent` 记录，会话仍可用（再 run 成功） | VM |
| 2 | 非可取消宿主调用 | 显式 timeout；随后进程组被清理；会话标记 poisoned；调用方收到干净错误 | native |
| 3 | 能力拒绝是类型化的 | 无宿主安装 → 类型化 `CapabilityDenied`（含 loom `capability-denied:` 映射），非泛型失败 | native |
| 4 | MCP 往返保留 RunId | `eval` 请求内 RunId 与响应信封 RunId 一致 | MCP |
| 5 | 会话在错误后存活 | 一次 `EvalError`（带 span）后，同一会话内新 run 成功 | session |
| 6 | 大整数精度 | 万位级大整数经 run 返回后等值（不做字符串往返） | VM |
| 7 | 输出捕获有界 | 超 `max_output_bytes` → 截断 + `OutputTruncated` 事件，非误杀 run | VM |
| 8 | 编译不执行用户代码 | 含顶层副作用（打印）的模块 `compile` 不产生 `output[]` | compile |

（表 6–8 为 ZIO-008/009 实现期验收；本设计验收 = 表 1–5 的合同可评审。对齐上游设计 §10 的矩阵风格。）

## 16. Open questions（留给 ZIO-008 定案）

- `fuel` 默认值：以现有 `step_limit` 默认折合还是另立常量。
- `max_memory` 精确堆上限值（native/JS 宿主参考 `contribs/native/js` 堆上限）。
- `max_output_bytes`：三条流共享一上限还是分设。
- `compile` 错误聚合（全量）还是首错即返。
- 会话内模块注册表的驱逐策略（长会话、多次 compile）。
- poison 粒度：会话级 vs run 级（§9 倾向会话级，代价是新会话）。
- `value` 经 MCP 传输时的精确表示编码（十进制字符串 + 元数据标记，ZIO-009 定）。
- `CapabilityDenied`/`Cancelled` 的 `isolation` 注记字段命名。
