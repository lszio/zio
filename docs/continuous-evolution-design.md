# 持续运行与自我进化：Groiz 技术对照及 Zio 设计补充

日期：2026-10-15。状态：**用户目标已明确，具体机制与自动采用策略为设计草案**。本轮为源码/合同评审，未执行训练、服务部署、版本发布或运行时测试；没有修改 Groiz。

评审基线：Zio `acdbd48cca70cac6301172442a101a61623b8134` 加本轮未提交设计文档；Groiz `33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46`，检查时工作区干净。下文 G1～G9 链接固定到该 Groiz 提交。

关联：[AI 交互设计](ai-language-interaction.md) · [决策 D22～D26](language-design-decisions.md) · [生态路线图](ecosystem-roadmap.md) · [为什么 restart 还不足以实现持续自我进化](../blog/07-continuous-evolution.md)。

用户进一步提出多 Groiz 实例联结、分工和讨论，见[群体协作设计](collective-intelligence-design.md)。它扩展本页的单实例持续演化：实例各自保留状态/版本/预算，以任务与证据协议协作；不把原有内部 population/worker 直接视为自主实例网络。

## 1. 结论：方向相容，当前设计还不充分

现有 Zio 的同像性、自举、查询/反射方向、condition/restart、执行 harness 与 MCP，适合作为可探索的语言底座。但它们不能单独保证一个应用长时间运行、训练模型、改变自身工具组织并持续变好。

需要补齐：持久任务与故障接管；程序/权重/脚手架共同版本化；训练状态与后端合同；候选的独立评价；运行中版本固定和迁移；采用策略、线上观察与回退。Groiz 已有其中一些实现和合同可供借鉴，但 canonical Zio 入口尚有接线缺口，不能将 Rust 库合同直接视为产品闭环。

用户目标可表达为：**一个持续服务的应用，在明确目标与资源范围内反复观察、提出改动、实验、评价并采用新版本；代码、模型和脚手架都是可演化对象，运行故障不会抹掉已完成的研究。**

“脚手架”暂按 Agent 的工具集合、工作流、记忆/上下文策略、模型路由、测试与资源声明理解，不只指新建项目模板。自动采用暂提出“预先授权范围内可自动验证并采用”的方案；是否启用、作用范围与发布条件仍待用户确认，本轮不改变任何现有权限或发布机制。

## 2. Groiz 的真实资产与未闭合部分

| 来源 | 源码/测试可见事实 | 可借鉴部分与限制 |
| --- | --- | --- |
| [G1 · 整体设计](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/docs/self-learning-architecture.md) | 定义代码 P、参数 θ、能力库 L；Task/Dataset/Recipe/Run、快照/检查点/分支、独立评价与治理；正式发布默认人工批准 | 复用领域模型与职责分离。该文有历史状态和规划 API，不是全部能力已实现的证据 |
| [G2 · coordinator.zio](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/coordinator.zio) | owner epoch、attempt lease、预算预约/计费、幂等账目和 branch head 预期版本更新 | 长期任务需要所有权、事实与账本；数据库仍须在实际服务中验证并发/崩溃一致性 |
| [G3 · runner.zio](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/runner.zio) | 入队、claim、worker 启动/轮询、checkpoint、取消、终态；旧 owner 的在途结果记录 unknown；有 checkpoint 时可标记 paused | 不把 queued 当已执行，不把未知结果当成功；需要真实重启/故障注入验收。API 当前有 owner tick 接线，不能沿用旧设计文档“完全未接 runner”的判断 |
| [G4 · checkpoint.zio](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/checkpoint.zio) 与 [worker.py](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/workers/torch/worker.py#L263) | 训练状态含参数、Adam 状态、CPU RNG、step；区分权重初始化和学习续接，按恢复级别检查兼容 | 检查点是训练合同，不是任意进程快照；当前 CPU/许可图范围不能扩称任意 LLM/GPU 训练 |
| [G5 · training.zio](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/training.zio) 与 [dual_learning_contract.rs](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/native/learning/tests/dual_learning_contract.rs) | 读取实际 worker 权重并建立模型快照；合同描述 Zio 结构改写加 CPU 训练；当前模块构造仍含 geometry/sensor 等固定语义 | 是代码/参数联合实验的参考，尚非通用模型或脚手架编译器；本轮未执行这些合同，环境不满足时部分测试会 skip |
| [G6 · logic.rs](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/native/learning/src/logic.rs) 与 [store publish](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/native/learning/src/store.rs#L1793) | LogicCandidate、范围比较、独立评价引用、HumanApproval 与版本冲突检查；发布指针在事务中更新 | 候选、评价、批准、发布是不同事实。当前为人工发布模型，不能把 Agent 身份冒充 HumanApproval 来实现自动采用 |
| [G7 · logic_contract.rs](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/native/learning/tests/logic_contract.rs#L745) | 有在途调用保留旧 ActiveLogic pin、后续调用读取新 pin 的合同 | 说明版本固定思路；pin 的值不变不等于所有实际长会话/闭包/模块缓存都已完成版本隔离 |
| [G8 · main.zio](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/main.zio#L120) | compare handler 生成示意指标；approve handler 渲染拟发布信息；run handler 明确拒绝并报告 provider wiring 缺失 | canonical 入口尚未完成相应模型调用/真实评价/采用闭环；不以旧 Rust 路径替代验收 |
| [G9 · api.zio](https://github.com/lszio/groiz/blob/33673b1e10ff8e5f43d244c4e1ed7a9d4e5c1d46/api.zio#L535) | select 的 publication_version 为 nil 并保存决策回执；approve 构造 publication/approval 并写回执；serve 中实际调用 runner tick | 在这些 handler 中未看到完整独立评价与发布事务调用。HTTP 200、回执或页面有版本号都不能证明已采用；本轮是静态核对，不推断线上运行结果 |

因此借鉴顺序应是：先把源文件、执行路径和验收证据一一对上，再复用稳定合同；不能只根据 README 或设计图声称 Groiz 已支持持续自主进化。

## 3. 职责分层：应用可以进化，语言保持通用

| 层 | 应承担什么 | 本次建议 |
| --- | --- | --- |
| Zio 核心/运行时 | 值/身份、版本化代码绑定、执行帧、condition/restart、GC roots、宿主调用与预算 | 提供长期运行和可替换代码的通用机制，不加入 `train-myself` 领域特殊形式 |
| 通用执行 harness | session/job/attempt、编译/执行、事件、取消、暂停、制品句柄 | 交互执行与持久任务共用身份和错误合同，生命周期明确区分 |
| Numa/训练 adapter | shape/dtype/语义空间、张量与模型后端、训练任务/检查点兼容 | Zio 持描述和编排，数值计算/自动微分由声明支持的后端执行 |
| Loom/脚手架库 | 模型调用、工具协议、工作流/上下文/记忆策略组合 | 可被候选重组的策略与固定宿主能力分开；复用同一语言执行 harness |
| Groiz 或新智能应用 | 目标、数据/反馈、候选、实验、评价、演化策略和采用政策 | 持续演化属于应用；现有 Groiz 可作为首个消费者，不把它并入语言 core |
| 持久与进程宿主 | 原子写入、数据库事务、worker 生命周期、时钟、资源限制、恢复/切换执行 | 可信机制保持小而明确；系统不靠“模型保证自己正确”维持一致性 |

Groiz 的具体领域表可以留在应用。只有两个实际消费者需要的通用 job/制品/能力接口才下沉到库，不复制第二套演化平台，也不把整个 Groiz Rust 业务直接搬进语言内核。

## 4. 三个可演化对象，一份可执行版本

### 4.1 代码、模型与脚手架

- **代码 P**：任务算法、Agent 决策、路由、数据转换及可变学习策略的版本化源码/AST/编译模块。
- **参数 θ**：模型权重、adapter、训练可变参数；以制品引用保存，不能因“代码即数据”就把 GB 级权重展开成 Lisp 字面量。
- **脚手架 H**：工具集合和签名、任务/工作流、模型路由、上下文/记忆构建、重试/恢复策略、并发/超时/缓存与资源需求。

L 是依赖/能力库的精确版本闭包；S 是可变业务状态。应用可描述为 `output = P(input; θ, H, L, S)`。这只是模型记法，不是新增 Zio 语法。

脚手架应有可读取、可比较、可组合的描述表示，执行时生成具体 handler/工作流。描述可以包含有界循环、分支和状态，不强迫任意 Agent 程序成为 DAG。编译后的脚手架只是一种制品，源码/规范描述保持权威来源。

### 4.2 ApplicationSnapshot（候选合同）

```text
ApplicationSnapshot
  schema + content identity
  entrypoint + code/source/compiled artifacts
  model artifacts + model/parameter manifests
  scaffold description + compiled scaffold artifact
  dependency lock + compiler/runtime/target compatibility
  state schema + migration/reset descriptors
  required capabilities + evaluation protocol references
  parent snapshot(s) + derivation/provenance
```

它固定能够共同执行的一组版本；不是把所有可变运行状态塞进一份文件。源码修订、编译器、模型、检索空间、工具 schema、依赖和状态 schema 的变化都会影响身份或兼容检查。评价结果作为独立记录引用 snapshot，不写进被评价制品后再改变其身份。

Checkpoint 另外引用快照并保存任务的可恢复进度；Publication 指针说明当前为新任务服务的是哪个快照；某个运行已固定的快照不因 publication 变化而漂移。

### 4.3 引用与回收

活跃发布、在途运行、暂停现场、有效 checkpoint 和显式保留的实验构成制品保留 roots。堆 GC 管内存对象，artifact GC 管磁盘制品，二者不同；不能在长任务仍引用旧权重时按“不是最新版”删除它。持久化 roots 的变更与回收之间需要明确事务/宽限边界。

## 5. 三种恢复、两个运行平面

| 机制 | 解决什么 | 保存什么与限制 |
| --- | --- | --- |
| condition/restart | 本次计算在有效恢复点继续 | 动态帧/handler/restart、必要对象仍活着；等待状态可超时；首轮不承诺进程崩溃后恢复 |
| durable job/checkpoint | 服务/worker 重启后继续工作 | 任务阶段、幂等键、进度、已提交制品、训练/搜索状态、已消费预算；依据恢复级别重新启动 |
| 版本切换与状态迁移 | 新代码/权重/脚手架开始服务 | 在途任务固定旧版，新任务固定新版；持久状态显式迁移或重置；切回代码不撤销外部动作 |

**服务平面**使用当前稳定快照处理真实请求；**研究平面**消费反馈，在独立 worker/session 里产生候选。应用控制面协调二者。训练或候选死循环不应占住唯一 HTTP/调度线程，也不能替换正在执行的关键控制帧。

建议的长任务状态是 queued → running → checkpointed/waiting → evaluating → qualified/rejected，错误/取消另记终态。waiting 若需要跨崩溃恢复，应把等待目标、截止时间和幂等条件持久化，不能只停一条 VM 栈等待未来事件。

对 Groiz 的 owner epoch/lease 借鉴为：worker 回执携 job/attempt/owner epoch，旧 owner 被 fence，重复结果与计费去重；失联但可能已产生外部动作的结果记 unknown，由查询/对账/补偿处理，不盲目重试。

进程恢复时不能复制一份旧预算重新花。长期服务可持续获得新的、显式配置的周期配额，但各 job/fork/resume 继承已消费账本；存储、模型调用、训练设备时间和并发槽位分别计量。

## 6. 三条演化回路

| 回路 | 改什么 | 固定什么、怎样判优 |
| --- | --- | --- |
| 代码优化 | 局部函数、算法、路由和允许的 Agent 决策 | 固定任务/输入合同、基线与外部评价；同时测正确性、回归、成本/延迟；只编译成功不够 |
| 模型训练 | 支持模型的参数、adapter，或经允许的结构 | 固定数据修订/分割、recipe、基础快照和 backend；权重改变与任务收益都要验证 |
| 脚手架组装 | 工具选择、执行拓扑、记忆/上下文、恢复/缓存/模型路由 | 固定工具合同与授权上限、外部任务评价；检查组合语义、终止/取消与资源需求 |

三条回路共享候选、制品、实验、评价、采用和经验模型；不同方法分别声明能力，不能用一个 `train(anything)` 接口隐去差异。

可先做单因素变更以归因，再做联合候选。代码结构变化可能让旧优化器状态不兼容，检索 encoder 变化可能让已有向量空间失效，工具组合变化可能引入新的效果需求。shape 一致、测试返回 success 或 embedding 相似都不足以证明兼容。

代码优化也不应只追求 token/LOC 变少。把一段长代码移入新工具或函数，评价时必须计入新增依赖、维护和运行成本；模型学习应同时比较质量与总训练/推理成本。

### 6.1 模型后端的能力表

至少分别声明 inference、evaluate、train、adapter-train、export、checkpoint/resume、cancel、支持设备/dtype/算子，以及模型/后端版本与数据使用条件。只有推理 API 的模型不能被标记为可训练。

最小验证从 Groiz 已有 CPU 小模型路径开始。真实完整续训通常还需 optimizer、scheduler、RNG、sampler/data cursor 等；某方法没用到的项可明确不适用，而不是漏字段。GPU 数值误差按声明容差验收，不把 CPU 轨迹回放直接推广为跨设备逐位复现。

训练资源是可终止 job，不在 Zio VM 内重写 PyTorch。Zio/Numa 描述计算与训练合同并引用后端制品，宿主负责实际计算和进程生命周期。

### 6.2 脚手架作为候选

例如把“每次调用大模型”改为“先查询本地库/缓存，仅不确定时调用模型”，就是脚手架变化。候选必须描述所用工具/模型/记忆版本、分支条件、失败恢复、输出合同、最大轮次与资源需求；compile/check 产出依赖和效果闭包，再在真实任务上比较。

“自行组装”允许在已授予工具集合与配额内自由组合。候选声明一种新工具不等于拥有它；新增宿主能力需独立授权。可编辑的重试、规划、上下文策略和固定的权限执行、资源计量、独立评价边界不能混成一个不可区分的 harness。

## 7. 采用与自治策略

Groiz 当前的人工批准是现行合同。用户的持续自我进化目标需要讨论是否允许在预先授权范围内自动采用；这是一项新策略，不能通过跳过 HumanApproval 或伪造 publisher 实现。

| 模式 | 自动范围 | 采用规则 |
| --- | --- | --- |
| 实验模式 | 自动生成、训练、组装、评价 | 不改变正式活动快照；人工选择是否采用 |
| 授权自治模式（建议，待确认） | 在约定区域/资源/任务内自动演化 | 独立评价通过且符合有效策略后，可自动 staged/canary/active；逐次留自动决策记录 |
| 扩大范围/修改治理 | 新能力、评分/预算策略、可信基线更新 | 独立变更流程；当前学习候选不能自行扩大本次授权或修改本次验收答案 |

自动 Decision 绑定 snapshot、evaluation refs、policy revision、预期旧 publication、范围与资源判定、过期时间和执行身份。HumanApproval 与 PolicyDecision 是不同记录，最终通过同一受控切换机制验证；候选可以提出策略改进，但新政策对何时/哪些实验生效必须独立决定。

不建议为了获得“完全自主”让提议器同时改目标、评价答案与发布条件。保持一部分稳定的任务评价与资源边界，才能判断进化是否改善了对外能力。稳定基线也可以经独立版本流程升级，不意味着永远不能改编译器或控制器。

### 7.1 新版怎样进入服务

1. 验证快照完整、兼容与评价证据，并校验采用决定对应当前基础版本。
2. 在新 context/worker 装配并预热候选，健康检查与迁移准备通过后才接新任务。
3. 在请求/任务一致性边界切换指针；旧任务保留旧依赖、模型与闭包，不原地 set! 全局函数名。
4. 可按声明支持的 shadow/canary 分配流量；有外部写操作的 shadow 默认走记录/模拟 adapter，不能重复真实扣款或写入。
5. 观察质量、错误、成本、内存与延迟，失败则停止新流量并按状态兼容策略回退；完成后旧版 drain/release。

模块 cache key 至少区分精确依赖版本/context；否则“旧任务固定 snapshot”仍可能加载到新模块。MCP session/run/对象句柄也需绑定 snapshot/revision，切换版本不能让旧句柄悄悄指向新对象。

数据库迁移先考虑 expand/contract 或兼容读写窗口。不可逆迁移和已发生的外部动作无法靠旧代码回滚；此类候选必须提供明确的恢复/补偿或采用限制，不能把“可切指针”当成整个世界可撤销。

## 8. 评价、经验与“持续变好”的定义

评价以固定任务合同和数据修订为依据，保留基线、公开验证集和独立验收。候选生成时能看到训练/公开反例，不能持续读取独立验收答案；大量候选反复查询同一验收集也会过拟合，需维护共享访问预算和更新策略。

记录失败、拒绝、负迁移和已知回归，不只记成功。提取的普通函数、宏、工作流片段或模型 adapter 成为新的版本化库候选，需在原任务回归和新任务上验证后再复用。

建议指标：固定预算下任务成功率、代码/模型/脚手架各自产生的收益、总研究成本、推理成本、恢复耗时、无效重复实验率、负迁移率、持续运行资源曲线。跨版本分数只在相同协议和资源口径下比较；更多训练步或更多工具不自动等于能力进步。

服务可以长期存在，但学习循环应可 idle/wait、按事件或周期唤醒、在配额耗尽时暂停；不靠无限忙循环持续调用模型制造“活着”的表象。

## 9. 缺口与工作包

| 工作包 | 补什么 | 与 Zio 路线关系及验收 |
| --- | --- | --- |
| EVO-01 canonical 闭环核对 | 把 Groiz 中模型调用、评价、批准/采用与真实事务接线对齐 | 是 Groiz 消费者任务，不修改其现有范围；回执必须能追到实际执行与 publication 变化，不能用旧 Rust 入口绕过 |
| EVO-02 长期任务/接管 | 持久 job/attempt、事件、epoch/lease、幂等与预算、故障后恢复 | R2/R3 与通用 harness；杀 owner/worker 后只能从已提交状态接管，未知结果可见，旧回执不覆盖新版 |
| EVO-03 联合版本/切换 | ApplicationSnapshot、精确模块绑定、状态 schema/迁移、预热/drain | R1/R2/R4；在途调用旧版完成，后续调用新版运行；旧依赖不漂移，回退符合状态合同 |
| EVO-04 模型训练 | Numa/后端能力表、异步训练 job、checkpoint 兼容与权重制品 | 官方库/host + 应用；CPU 最小模型先真实降低固定任务损失，续训状态完整，未支持训练的模型明确拒绝 |
| EVO-05 脚手架候选 | 工具/工作流/上下文/路由描述、编译与效果闭包、版本锁 | Loom/项目工具 + 应用；自行产生两种可执行组合，比较任务收益，不增加未授权能力 |
| EVO-06 采用策略/线上观察 | 明确人/策略决策、独立评价、分阶段采用、故障回退 | Groiz 或新应用所有；自治策略未采纳前维持现行人工边界；过期/陈旧/错候选的决定拒绝 |
| EVO-07 持续演化样板 | 服务平面 + 三条研究回路 + 长时间故障演练 | R5 门槛；固定资源内完成真实改进、重启接续、失败候选隔离、旧版可用与资源释放 |

推荐顺序：核对 EVO-01，同时确定 EVO-03 的版本/状态合同 → EVO-02 与 CPU 模型回路 → 代码及脚手架候选 → EVO-06 策略采用 → EVO-07。AI-01～04 的查询/实验/MCP 可提供操作表面；不需要等所有远程 restart 或完整 MOP 才能开始 durable job 原型。

### 首个端到端样板

在固定的任务集上，系统先用基线持续回答请求，收集可见失败；自行提出一个局部代码改进、训练一个小模型或 adapter、重组一条工具工作流。分别评价三种变更，再选一项合格候选形成完整快照，按选定策略采用。

演示中至少发生一次 worker 中断、一次 owner 重启、一个不合格候选和一个陈旧采用请求。必须观察：任务账本延续、未提交半成品不入历史、在途请求没有混版、前置副作用没有盲目重放、失败不会覆盖活动版。采用后还要证明固定评价上的收益与服务资源稳定；只展示新文件或更低训练 loss 不够。

时间尺度应先声明测试窗口和负载，并记录 heap、磁盘、worker、handle、队列与 checkpoint 的增长/回收；测试一小时不能冒称无限运行保证。阶段性扩大 soak 范围，按测得缺口改设计。

## 10. 对当前设计的判定

| 用户目标 | 当前判定 |
| --- | --- |
| AI 理解、执行、探索语言 | AI-01～06 设计已覆盖主要交互；实现仍待完成 |
| 持续运行与故障恢复 | 现有镜像/GC/预算方向不足，需 EVO-02 与持久状态/版本合同 |
| 自己优化代码 | 同像性/编译器适合作为基础，缺候选范围、独立评价、采用与版本固定的端到端机制 |
| 自己训练模型 | Groiz 有 CPU 参考路径；Zio 需 backend/job/数据/状态接口，不能推断通用训练已具备 |
| 自己组装脚手架 | 需把 H 变成可验证/可版本化的对象，允许重组策略而保留宿主执行边界 |
| 自主采用改进 | 需要明确采用政策；Groiz 现有人工批准与自动采用不能混写，本轮不启用自动发布 |

这些属于语言生态应支持的长期应用能力，不应只列为最后一阶段的“AI 库加分项”。可先把 Groiz 作为需求与验收消费者，逐项检验 Zio 的通用机制是否真的足够。

## 11. 调研方式与限制

GitNexus 对 `zio` 使用当前基线索引；Groiz 原仓库不在已注册图中，故把干净提交导出到 `/tmp/zio-groiz-review` 并以 `groiz-review` 建立图：3,016 nodes、8,961 edges、264 flows。概念 query 与 `publish_approved`/`save_state` context 用于定位调用/依赖；图对 Zio 脚本及动态宿主调用没有完整覆盖，相关入口进一步逐段读取，不用零调用推断“没用”。

所有“已有”指本轮能定位到的源码或合同；没有重新执行 Groiz 测试，不把其历史测量、skip 条件、设计里的完成描述升级为本次运行结论。本文提出的通用接口、脚手架编译与自治采用均未实现，正式功能状态仍以各仓库的真实验收为准。
