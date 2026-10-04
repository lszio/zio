# grove Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `subagent-driven-development` or `executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> 命名以整体设计为准：产品/命令 `grove`，学习宿主库 crate `grove`，产品 crate `grove-app`。保留现有文档路径和按职责命名的目录，不重命名既有 Zio 组件。

**Goal:** 交付可嵌入的 Zio 自学习库及本地产品，实现多模态与多源反馈、代码/权重联合学习、检查点历史、分支群体和模块协同演化。

**Architecture:** Zio 持有模型程序与学习策略；Rust 宿主提供合同校验、持久化、资源与权限边界；PyTorch 工作进程执行真实张量训练。产品通过同一库接口提供 CLI、HTTP 与可视界面，不复制学习逻辑。

**Tech Stack:** 现有 Rust workspace、Zio 和 zio-ai；拟新增 SQLite/内容寻址文件、Python/PyTorch worker、Rust/Axum API 和原生 HTML/CSS/JS。新增依赖在实施时固定兼容版本，当前不宣称已安装。

---

## 1. 状态与使用方式

- 本计划为待执行的整体工作包计划，不是已实现代码，也不是已完成实验报告。
- 唯一架构依据：[整体设计](../../self-learning-architecture.md)。既有数值学习状态仍以 [特性矩阵](../../feature-matrix.md) 为准。
- 范围涵盖全部已讨论的用户能力；P1–P4 是交付顺序，不是只交付 P1 的范围缩减。
- 每个工作包提供目标文件、合同、行为测试和实际运行证据。文中的新命令、新文件和测试名是实施目标，不能在文件尚未建立时当作可执行现状。
- 不在规划阶段预写整套实现。执行每个工作包时先读取受影响代码与宿主合同；修改现有符号前完成仓库要求的影响分析，使用可用 LSP 查引用；提交前执行影响范围检测。
- 每个包按“建立关键失败行为检查 → 最小实现 → 定向检查 → 实际路径 smoke → 更新状态文档”执行。测试只保护消费者可见的行为和安全不变量，不建立字符串/私有实现锁定测试。
- 不自动提交本次文档或开始实现。各工作包实现完成后可形成独立审查提交，不能以占位 API、模拟训练、伪造 worker 结果作为交付。

## 2. 已核实的集成起点

| 现有文件 | 可复用事实 | 实施约束 |
|---|---|---|
| `Cargo.toml` | 当前 workspace 为 core、cli、ai | 新增宿主/产品时同步 workspace 与状态生成脚本 |
| `ai/src/lib.rs` | `LlmHost`、`EmbedHost`、外部 `install` | 复用 attach 方式；多模态与训练不是已有能力 |
| `ai/src/http.rs` | opt-in HTTP、阻塞请求、超时与响应大小上限 | 教师适配复用传输边界，不破坏无网络默认构建 |
| `cli/src/main.rs` | 私有 context 装配、脚本执行、`--llm-replay` | 不向 REPL 塞产品状态；现有 CLI 保留回归验证 |
| `lib/zio/learn.zio` | 数值候选白名单、评分和预算 | 扩展为通用任务时共享循环，保留数值任务本身，不留下第二套旧引擎 |
| `lib/zio/proposer.zio` | 候选文本与历史协议 | 教师标签和候选代码分开，禁止教师直接批准候选 |
| `ai/tests/` | AI、HTTP、proposer、L3 合同 | 修改真实合同就迁移全部消费者；无需变更的文本能力不人为移除 |
| `site/` | 展示页和 REPL，不是学习控制台 | 新产品界面放 `app/web/`，不把 landing page 当产品完成 |

## 3. 文件与职责地图

下列新文件按工作包需要创建，不提前生成空目录、空模块或实现占位。目录名表示工作边界，子模块只有在实际使用时才注册。

| 文件/目录 | 状态 | 单一职责 |
|---|---|---|
| `lib/zio/learn.zio`、`lib/zio/proposer.zio` | 修改已有 | 学习 driver、任务适配、提议协议与现有数值任务 |
| `lib/zio/learn/model.zio` | 新增 | 模块组合描述与候选变更构造 |
| `lib/zio/learn/feedback.zio` | 新增 | 信号使用政策、冲突裁决与数据视图构造 |
| `lib/zio/learn/recipes.zio` | 新增 | 学习方法配方及预算内的训练/搜索组合 |
| `lib/zio/learn/population.zio` | 新增 | 分支选择、多样性、预算分配建议 |
| `lib/zio/memory.zio`、`lib/zio/vector.zio` | 新增，沿用原规划名 | 经验检索、函数抽象；可选语义向量索引 |
| `ai/src/teacher.rs` | 新增 | 教师能力与多模态请求/输出协议 |
| `ai/src/http.rs`、`ai/src/lib.rs` | 修改已有 | 教师供应商适配、导出；保留既有文本能力 |
| `learning/Cargo.toml`、`learning/src/lib.rs` | 新增 | `grove` crate 与 Zio 安装入口 |
| `learning/src/contracts.rs` | 新增 | 有版本的合同、统一错误、身份与权限上下文 |
| `learning/src/store.rs`、`learning/src/artifacts.rs` | 新增 | SQLite 事务与不可变制品完整性 |
| `learning/src/worker.rs` | 新增 | 受控进程协议、隔离和生命周期 |
| `learning/src/checkpoint.rs` | 新增 | 一致性提交、恢复级别、兼容校验 |
| `learning/src/evaluation.rs` | 新增 | 固定评价协议的执行与证据记录，不替代任务评价政策 |
| `learning/src/coordinator.rs` | 新增 | 单协调者队列、租约、预算账本与原子 head 更新 |
| `learning/src/publication.rs` | 新增 | 权限、资格、版本冲突与发布指针事务 |
| `workers/torch/worker.py`、`graph.py`、`recipes.py` | 新增 | 协议入口、许可张量图、后端训练实现 |
| `workers/torch/requirements.txt` | 新增 | 可复现安装的固定版本依赖 |
| `app/Cargo.toml`、`app/src/main.rs` | 新增 | `grove-app`，二进制 `grove` 的装配和子命令 |
| `app/src/api.rs` | 新增 | 同源 API、认证授权、幂等与状态查询 |
| `app/web/index.html`、`app.js`、`styles.css` | 新增 | 观察纠正、历史比较、群体/模块与发布界面 |
| `examples/self-learning/` | 新增 | 真实技术验收数据生成、任务合同和 Zio 模型/学习入口 |
| `learning/tests/`、`app/tests/`、`workers/torch/tests/` | 新增 | 合同、安全边界与端到端行为回归 |

拟新增依赖仅在使用时引入：宿主的 SQLite、serde/JSON、SHA-256 与进程限制依赖；产品的 Axum/runtime；worker 的 torch 及非执行式张量存储。精确版本由目标 Rust/Python/设备环境确定并锁定，不凭空承诺兼容矩阵。

## 4. 技术验收任务与真实业务边界

### 4.1 可独立运行的验收任务

使用自生成的“几何图像 + 数值传感器”分类任务：16×16 图像包含水平/垂直图形，结构化输入提供带噪声的正/负读数，标签由图形类别与读数符号的 XOR 关系确定。亮度、位置和噪声作为扰动；以潜在场景 ID 分组划分训练、验证和独立验收，派生图不得跨集合。

- 初始学生：线性融合，不能可靠表示 XOR；结构候选允许引入受限非线性隐藏层，内循环真实训练其参数。
- 缺失模态有 mask；无法由剩余信息确定的样本允许显式 abstain，不能以补零伪造完整输入。准确率与覆盖率分别报告。
- 小型本地神经教师在独立训练来源上训练并冻结，通过真实 HTTP 教师接口提供输出；来源标注 local-teacher。它不是外部商业模型，也不是固定答案回显。
- 模拟人类操作的自动检查用于回归；产品验收还必须在实际界面人工提交纠正。纠正教师伪标签后冻结新数据视图，不能把验收标签输送给教师或学生。
- CPU 小模型使用确定性设置与固定数据，在执行 W00 后把数据摘要、样本数、训练步预算和阈值固定到验收合同。此任务仅证明工程闭环，不证明真实应用收益。

### 4.2 真实业务接入条件

业务使用需给出数据与训练许可、可独立核实的结果反馈、教师凭据/许可、资源预算以及明确质量门槛。不具备这些条件时，技术能力可以交付，真实业务成效必须标为未验证；不使用未知业务数据编造提升幅度。

## 5. 阶段与依赖

| 阶段 | 工作包 | 必须交付的结果 |
|---|---|---|
| P1 多源证据与模型身份 | W00–W03 | 可持久化模型身份、真实观察/反馈链、教师与纠正数据视图、完整性检查 |
| P2 双学习、检查点与历史选择 | W04–W08 | 真权重训练、真代码变更、学习续接、分裂、统一比较和本地端到端 CLI |
| P3 群体与产品 | W09–W12 | 实际多 worker、统一预算、带权限的 API、可交互产品、失效和保留处理 |
| P4 模块协作与经验 | W13–W17 | 模块组合、三索引经验、扩展学习方法、专家协作、整体验收与交付 |

依赖以包为单位：W00 → W01 → W02；W03 依赖 W01/W02；W04 依赖 W00/W01；W05 依赖 W02/W04；W06 依赖 W04/W05；W07 依赖 W01/W06；W08 依赖 W03/W05/W06/W07；W09 依赖 W08；W10 依赖 W08/W09；W11 依赖 W10；W12 依赖 W06/W10；W13 依赖 W07/W09；W14 依赖 W02/W13；W15 依赖 W03/W04/W07；W16 依赖 W13/W15；W17 依赖 W11/W12/W14/W16。

W03 与 W04、W11 与 W12、W14 与 W15 在共享合同稳定后可分工并行；这是执行规划，不在文档编写阶段启动开发代理。每个共享文件由一个集成责任者合并，不能让并行工作分别重写公共合同。

## 6. P1：证据、模型与学习入口

### W00 — 冻结验收合同与运行基线

**文件：** 新增 `examples/self-learning/generate.py`、`task.json`、`protocol.json`；更新 `docs/adrs.md` 记录经评审的 ADR-016 扩展，不伪造已采纳状态。

- [x] 定义图形/数值任务、组级数据分割、缺失模式、指标和 abstain 合同；生成数据及摘要清单，评价标签不进入训练导出路径。
- [x] 建立最小消费者检查：同场景派生样本不跨集合；缺失模态的真值/可判定标记符合任务定义；同种子重建数据摘要一致。
- [x] 固定 CPU 验收配置：训练 512、验证 128、独立验收 256 个基础场景；扰动变体按原场景归组。记录数据与训练环境版本。固定随机计划为 3 次独立种子运行。
- [x] 技术成功门槛：完整输入上双学习候选验收准确率至少 90%，相对线性基线平均提升至少 15 个百分点；缺失输入单独报告覆盖率与已回答准确率，不混入完整输入指标。这些是待达成门槛，不是已测结果；不得在看到验收结果后降低门槛。
- [x] 验证命令：`python examples/self-learning/generate.py --self-check`。要求退出 0，并报告分组泄漏为 0；再执行已有 `cargo run -p zio-cli -- examples/learn-demo.zio` 保留旧数值功能证据。

### W01 — 模型合同、事务存储和制品身份

**文件：** 新增 `learning/Cargo.toml`、`learning/src/lib.rs`、`contracts.rs`、`store.rs`、`artifacts.rs`、`learning/tests/store_contract.rs`；修改根 `Cargo.toml`、`tools/project-status.sh`。

- [x] 落实 ModelSnapshot、Observation、Prediction、LearningSignal、DatasetRevision、Recipe、Run、Checkpoint、Branch、Population、EvaluationRecord 的 schema-version 与引用边界；字段依据整体设计，不创造第二套别名。
- [x] 首先检查：不同主体不能引用对方制品；内容损坏被拒绝；重复信号不重计；半写入制品不可成为已提交检查点；未知 schema 不执行。
- [x] 用 SQLite 事务维护引用与版本；不可变内容先持久化，再提交清单。分支 head/发布指针采用 expected-version 比较更新。失败不能产生悬空已提交引用。
- [x] 注册实际使用的外部 attach 能力；未装配能力显式失败，不在 core 添字段。存储阶段不能假装已有训练恢复能力。
- [x] 执行 `cargo test -p grove --test store_contract`；在临时目录真实写入、杀掉写入进程、重新打开存储，观察提交记录仍完整且半成品不可见。完成后更新特性矩阵中实际覆盖的细分能力。

### W02 — 人类纠正、信号生命周期与冻结数据视图

**文件：** 新增 `lib/zio/learn/feedback.zio`、`learning/tests/feedback_contract.rs`；修改 `learning/src/contracts.rs`、`store.rs`。

- [x] 通过库入口提交观察、预测、教师伪标签、局部纠正、偏好、演示、环境结果、修订和撤回；检查目标与模型版本关联，不从当前 head 猜测历史预测。
- [x] 先检查：纠正只影响目标字段；授权人类纠正替代同目标伪标签；相互冲突的人类意见隔离；弃权不是负标签；迟到结果关联原动作；撤回进入下一个视图而不改变正在训练的冻结视图。
- [x] 由版本化反馈政策生成 DatasetRevision，记录分割、来源、使用许可与转换规则；同主体幂等键重复不重复收录。
- [x] 执行 `cargo test -p grove --test feedback_contract`。实际持久化一条预测、两次冲突纠正、一次裁决与撤回，重启后验证视图成员和冲突状态与预期一致。

### W03 — 教师接口与多模态适配

**文件：** 新增 `ai/src/teacher.rs`、`ai/tests/teacher_contract.rs`、`examples/self-learning/teacher.py`；修改 `ai/src/lib.rs`、`http.rs`、`ai/Cargo.toml`。

- [x] 定义教师能力：输入模态、输出 schema、硬/软输出支持、版本、预算、数据保留/训练许可；内容块使用授权制品引用，宿主负责必要编码。
- [x] 在协议级检查未授权外传、超长响应、超时、错误 schema、虚假 logits 能力；教师输出代码只能作为候选，不能被自动执行。
- [x] 复用现有 HTTP 超时与响应限制。新增教师协议和文本 completion 角色分开；若调整现有导出符号，先查全部引用并一次迁移，不保留过渡别名。
- [x] 实现本地教师服务装配和请求记录；W04 完成后加载真实训练并冻结的教师权重，不以常量标签假冒模型。无教师凭据时报告缺失能力，不悄悄换成本地教师。
- [x] 执行 `cargo test -p zio-ai --test teacher_contract --features http`。本地 HTTP 的真实神经教师调用由 W08 端到端验收；外部供应商联调需显式凭据和许可，单独标注是否执行。

## 7. P2：真实学习、恢复与比较

### W04 — PyTorch worker 与隔离执行

**文件：** 新增 `learning/src/worker.rs`、`learning/tests/worker_contract.rs`、`workers/torch/worker.py`、`graph.py`、`recipes.py`、`requirements.txt`、`workers/torch/tests/test_training.py`。

- [x] 定义协议版本、请求/运行/尝试 ID、能力握手、制品输入输出、进度、完成与失败消息。限制帧大小；stdout 不写日志，stderr 限量；取消回收整个进程树。
- [x] 实现许可图算子：输入/缺失 mask、归一化、拼接、线性、ReLU、softmax 与分类损失；形状/dtype/语义空间检查在执行前完成。图来自 Zio 模型描述，不接受任意 Python 源码。
- [x] 先检查：参数更新使保留样本损失下降且预测有实际变化；非法算子、损坏权重和不兼容形状拒绝；超时进程被终止；随机训练在声明容差内可重复。
- [x] 宿主执行 Linux 受限工作进程、只读/受控写入制品根、网络禁止和资源上限；不支持隔离能力的环境 fail closed，不降级到无限制执行。不可信张量制品采用非执行式格式，不加载任意 pickle 对象。
- [x] 执行 `python -m unittest discover -s workers/torch/tests -p test_training.py` 与 `cargo test -p grove --test worker_contract`；实际 CPU 训练 W00 数据，保存模型并在新进程载入预测。CPU 是必验平台，GPU 实测另记设备与结果。

### W05 — 代码与权重联合学习 driver

**文件：** 修改 `lib/zio/learn.zio`、`proposer.zio`；新增 `lib/zio/learn/model.zio`、`recipes.zio`、`learning/tests/dual_learning_contract.rs`；按合同变化迁移 `ai/tests/learn3_contract.rs` 和现有脚本调用。

- [x] 提取共享的预算、候选生命周期与反馈 driver；已有数值学习成为任务适配，不复制一个无限制新循环。现有数值任务仍必须输出真实可执行表达式。
- [x] 建立 Candidate 合同：基础版本、允许的代码变更、模块/参数制品和来源。解析、展开后检查、依赖闭包和权限检查适用于所有 proposer。
- [x] 先检查：非法 AST 无法越权；训练失败只淘汰本候选；耗尽训练/搜索预算停止；固定结构可训练，结构变化后新参数初始化且旧模型不变。
- [x] 实现受限的模型结构变换，至少包含线性融合到非线性融合的真实 AST 改写；内循环调用 worker 训练参数，评价只消费实际执行结果。
- [x] 执行 `cargo test -p grove --test dual_learning_contract`，运行 W00 完整输入任务，记录固定基线、仅训练参数、代码与参数联合三组结果。未达 W00 门槛时保留诊断，不修改测试为“能跑即可”。

### W06 — 检查点、暂停、恢复与分裂

**文件：** 新增 `learning/src/checkpoint.rs`、`learning/tests/checkpoint_contract.rs`；修改 `store.rs`、`worker.rs` 与训练 worker 状态导出。

- [x] 定义一致性边界，保存模型、优化器/调度/缩放、RNG、采样器位置、搜索历史/代际、配置和预算账本；多 worker 状态不得跨训练步拼接。
- [x] 先检查：中断保存不会出现可恢复半成品；缺失制品与版本不兼容拒绝；resume 不重置预算；fork 不修改父权重；未声明确定性环境不承诺逐位重现。
- [x] 支持模型初始化、学习续接、受控回放三种级别。暂停保存成功才进入 paused；恢复创建关联新运行，外部响应未知时不自动声称可重放。
- [x] 执行 `cargo test -p grove --test checkpoint_contract`；在确定性 CPU 任务第 N 步保存，结束进程、恢复到 N+K，与不中断训练比较采样序列、步数、损失和参数容差。再从同一父节点创建两条分支，确认独立演化。

### W07 — 历史重评、选择与发布资格

**文件：** 新增 `learning/src/evaluation.rs`、`publication.rs`、`learning/tests/evaluation_contract.rs`；修改 Zio recipe/选择政策与存储引用。

- [x] 固定 EvaluationProtocol 的数据、任务、种子、错误/超时处理、硬件口径；同一快照可有多份评价记录，分数不写回模型身份。
- [x] 先检查：不同协议结果不可混排；超时计入失败；不满足硬门槛的高平均分模型不能发布；陈旧 expected-version 不覆盖当前发布；选择历史版本不改变其原始记录。
- [x] 实现单冠军及质量/成本非支配候选选择；按场景保留专长，单独呈现训练研究成本和部署成本。独立验收访问预算在所有分支间共享。
- [x] 执行 `cargo test -p grove --test evaluation_contract`；将旧线性模型、仅调参模型、结构改进模型在同一协议重跑，检查比较报告来源与实际预测一致，再演示拒绝低质量候选、选择可用历史版本。

### W08 — 库调用与本地端到端命令

**文件：** 新增 `app/Cargo.toml`、`app/src/main.rs`、`app/tests/learning_e2e.rs`、`examples/self-learning/run.zio`；修改根 `Cargo.toml` 与 `tools/project-status.sh`。

- [x] 二进制命名 `grove`，提供 `demo`、`inspect`、`checkpoint`、`fork`、`resume`、`compare`、`select` 命令，命令参数映射已有库合同；禁止 CLI 私自重写训练/选择逻辑。
- [x] 为 Zio 脚本安装可信能力与 stdlib，复用公共 reader/eval 和外部 attach 模式；上下文所有权显式，不依赖跨线程共享解释器。
- [x] 实现 `demo --case dual --root PATH --device cpu`：生成技术数据、训练/冻结本地教师、调用教师、提交纠正、冻结视图、训练结构候选、保存/恢复、比较并输出报告。选不出达标候选时明确失败，不强行晋升。
- [x] 执行 `cargo test -p grove-app --test learning_e2e` 和 `cargo run -p grove-app --bin grove -- demo --case dual --root /tmp/grove-dual --device cpu`。要求真实进程、真实权重与预测、来源明确；不得只断言返回对象非空。

## 8. P3：群体与产品

### W09 — 多 worker 群体协调与全局预算

**文件：** 新增 `learning/src/coordinator.rs`、`lib/zio/learn/population.zio`、`learning/tests/population_contract.rs`；扩展 demo 的 `population` 场景。

- [x] 单协调者管理预算、执行槽位、租约和 attempt ID；Zio 策略提出分配/选择建议，宿主事务执行。串行交错与真正并行共用分支合同。
- [x] 先检查：fork 不放大预算；旧 worker 回执不能覆盖新 head；失败分支不杀死其他分支；重复消息不重复计费；未知外部付费调用结果保持可见而非假定恰好一次。
- [x] 实现探索配额、按资源档位比较、保留基线/优势/差异分支。达到预算后停止新工作，已运行任务按配置在安全点暂停或取消。
- [x] 执行 `cargo test -p grove --test population_contract`；实际启动两个训练进程，记录重叠执行时间，终止一个再恢复，验证另一条继续、全局账本连续、过期结果被拒绝。
- [x] 实际场景命令：`cargo run -p grove-app --bin grove -- demo --case population --workers 2 --root /tmp/grove-population --device cpu`。必须观察真实并行，不用异步返回 ID 代替并行证明。

### W10 — 产品 API 与授权发布

**文件：** 新增 `app/src/api.rs`、`app/tests/api_contract.rs`；修改 app 装配和依赖。

- [x] 增加 `serve --bind 127.0.0.1:PORT --root PATH`，同源 API 映射 observe/predict/signal/teacher/learning/checkpoint/fork/resume/pause/cancel/compare/select/population/compose/promote；耗时返回运行引用。
- [x] 先检查 reader 无法纠正、annotator 无法训练或发布、operator 无法绕过 publisher、错误来源/令牌被拒绝、重复修改请求幂等、冲突 head 明确报错。
- [x] 提供状态查询与带关联 ID 的事件读取，敏感原始内容与密钥不进入普通日志。默认拒绝无鉴权的非 loopback 绑定，不默认承担多租户公网托管。
- [x] 执行 `cargo test -p grove-app --test api_contract`；启动实际服务，使用真实 HTTP 完成一次观察、预测、局部纠正、启动学习、查看结果、授权发布；验证未授权同样操作确实被拒绝。

### W11 — 产品界面：纠错、谱系、比较与模块视图

**文件：** 新增 `app/web/index.html`、`app.js`、`styles.css`；由 `app/src/api.rs` 提供同源静态资源。

- [x] 实现任务/观察与反馈页，展示具体预测版本、字段/区域纠正范围、冲突和裁决；反馈提交与模型已学习分开显示。
- [x] 实现运行及谱系历史、检查点恢复级别、从节点分裂、暂停/恢复、同协议比较、候选选择和发布确认。清理或撤回的节点标为不可恢复/失效。
- [x] 实现模块依赖与差异表，展示参数/结构改变、共享参数约束和组合状态；P4 组合能力未到位时不能展示成功按钮假装功能存在。
- [x] 保留键盘操作、表单标签、状态文本和危险操作确认。先做可读列表与展开树，不引入图形编辑器或拖拽网络编排依赖。
- [x] 启动真实服务，用浏览器完成“预测 → 人工纠正 → 新分支 → 训练 → 比较 → 授权发布/拒绝”。截图记录谱系、比较和最终运行状态；浏览器验收是必须证据，不以 DOM 源码检查替代。

### W12 — 撤回、保留与故障恢复

**文件：** 修改 `learning/src/store.rs`、`artifacts.rs`、`checkpoint.rs`、`publication.rs`；新增 `learning/tests/lifecycle_contract.rs`；扩展产品的失效/保留展示。

- [x] 先检查：活跃部署和固定保留节点的依赖不被删除；撤回信号影响的快照被标记；已清理历史不可伪装可恢复；旧模型回滚不会重放外部动作。
- [x] 实现引用可达性与保留政策，清理只处理未引用且符合期限的制品；权限性数据删除优先于可重现性，保留最小允许元数据。
- [x] 进程重启重新获取协调者所有权、回收过期租约，不能把旧 worker 当作当前尝试；数据库/制品备份与恢复检查依赖完整性。
- [x] 执行 `cargo test -p grove --test lifecycle_contract`；真实服务重启后查看历史、恢复有效分支，撤回训练数据并验证相关节点失效；主动损坏副本制品时返回 artifact-unavailable 而不是继续发布。

## 9. P4：模块、经验与多种学习方法

### W13 — 模块分支、组合与联合微调

**文件：** 修改 `lib/zio/learn/model.zio`、`learning/src/contracts.rs` 和张量图编译；新增 `learning/tests/composition_contract.rs`。

- [x] 模块快照绑定输入空间、参数/梯度合同、冻结上下文和依赖；共享参数/联合优化器/不可分离状态构成同一演化单元。
- [x] 先检查：同维不同语义空间不可直接连接；共享参数不能各自恢复为两个冲突版本；模块权限闭包不因组合丢失；组合后性能退化不能借局部高分晋升。
- [x] 实现局部替换、权重兼容迁移、重初始化和联合微调。组合生成新快照及多父谱系，不修改原分支；不提供任意权重平均。
- [x] 执行 `cargo test -p grove --test composition_contract`；分别演化视觉与融合模块，再将组合体进行整体训练/评价，比较原模型、两个局部候选和组合体，保留真实退化结果。

### W14 — 三索引经验、函数抽象与复用

**文件：** 新增 `lib/zio/memory.zio`、`vector.zio`、`learning/tests/memory_contract.rs`；扩展已有经验存储字段而不是再建独立事实库。

- [x] 实现 AST 结构检索、共同探针下的行为指纹和可选语义向量；向量记录编码器/空间版本，空间变化必须重建或显式迁移。
- [x] 先检查：语义近邻不能合并不同 AST；不同探针不能混比；检索不可越过数据许可；撤回来源的能力不可静默加载。
- [x] 从多个已验证程序提取最小公共抽象，普通函数优先；候选抽象替换后通过原任务回归与新任务评价，才晋升为能力模块。宏需要展开后再次检查。
- [x] 执行 `cargo test -p grove --test memory_contract`；实际在未参与抽象发现的任务上比较搜索调用、质量、成本与包含库定义成本的总描述长度，不只报告函数名变短。

### W15 — 扩展 recipe：软蒸馏、偏好、演示与自监督

**文件：** 修改 `lib/zio/learn/recipes.zio`、`workers/torch/recipes.py`；新增 `workers/torch/tests/test_recipes.py`、`learning/tests/recipe_contract.rs`。

- [x] 在已有监督/硬输出蒸馏之上实现：同类空间上的温度分布蒸馏；成对输出的排序损失；合法动作空间内的演示行为克隆；配对模态对齐或遮蔽重建的自监督 recipe。来源适配器不因 recipe 改变而重写。
- [x] 环境反馈首先支持延迟结果监督；需要奖励学习的受限离散策略提供有显式动作/奖励/终止合同的策略梯度 recipe，不宣称覆盖任意强化学习环境。验证器反例进入结构修复，不塞成统一奖励。
- [x] 先检查：词表/类别不匹配拒绝软蒸馏；偏好弃权不计为输；非法动作 mask 不被训练绕过；延迟奖励归属原轨迹；自监督改善不能跳过任务验收。
- [x] 执行 `python -m unittest discover -s workers/torch/tests -p test_recipes.py` 和 `cargo test -p grove --test recipe_contract`；每种 recipe 用小型可核实任务真实更新参数并比较留出行为。外部供应商未提供软输出时只报告硬蒸馏已联调。

### W16 — 专家路由、输出集成与多教师蒸馏

**文件：** 修改 `lib/zio/learn/model.zio`、`recipes.zio`、`population.zio` 与产品模块界面；新增 `learning/tests/ensemble_contract.rs`。

- [x] 将路由器、专家版本、组合规则和依赖绑定为一个 ModelSnapshot；记录选错专家、不可用专家、缺失模态和总成本的行为。
- [x] 先检查：不兼容输出空间无法集成；没有可用专家时显式失败/abstain 而非伪造输出；群体一致错误不会自动成为真标签；多专家调用不能绕过总预算。
- [x] 实现按合同的专家路由、输出投票/加权以及多教师学生蒸馏。迁移结果走同一学习与验收流程，不把多数票作为最终验收依据。
- [x] 执行 `cargo test -p grove --test ensemble_contract`；对同一留出集比较最佳单模型、路由/集成和蒸馏学生，报告质量、覆盖率、尾延迟与完整资源成本。组合策略不达标时保留单模型发布。

### W17 — 全链路交付、打包与证据归档

**文件：** 扩展 `app/tests/learning_e2e.rs`、demo 的 `modular` 场景；更新 `README.md`、`docs/feature-matrix.md`、`docs/roadmap.md`、`docs/adrs.md`、`tools/project-status.sh` 和本计划状态。

- [x] 固定 Rust 锁文件和 Python 依赖安装方式，提供库嵌入、本地 CLI、产品启动及数据目录说明；不自动下载来源不明的权重，不把网络凭据写入模型或演示制品。
- [x] 分别验证无训练后端的普通 Zio CLI、具备 CPU 后端的完整产品、权限受限运行；缺能力必须明确失败。已有数值 demo 与 AI 合同不退化。
- [x] 执行完整消费者回归：`cargo test --workspace --all-features`、`python -m unittest discover -s workers/torch/tests`；运行 dual、population、modular 三个实际 demo 场景，再以浏览器走通纠正与发布路径。
- [x] 对照第 10 节逐项归档制品摘要、预测/评价记录、进程运行证据、截图和未验证外部环境。只将通过真实验收的能力从 Planned 改为 Experimental；不得批量标 Stable。
- [x] 最终报告区分技术验收结果、真实业务结果、CPU/GPU、上游商业服务/本地教师。未提供业务数据或外部凭据不会被伪造为已经完成联调。

## 10. 需求—工作包—证据映射

| 需求/架构合同 | 工作包 | 必须看见的证据 |
|---|---|---|
| 作为 Zio 库与独立产品、core 最小 | W01/W05/W08/W10/W17 | 脚本与产品调用同一逻辑；core 不依赖学习/产品 |
| 多模态统一输入及模态缺失 | W00/W03/W04/W08 | 图像和数值真实进入学生；缺失 mask 与 abstain 正确 |
| 上游模型硬/软蒸馏 | W03/W08/W15 | 真实教师响应、来源/许可、真实学生训练；软输出条件明确 |
| 人类纠正、偏好、冲突和撤回 | W02/W11/W12/W15 | 版本关联、局部纠正、冲突隔离、实际 UI 操作和失效传播 |
| 代码与权重联合学习 | W04/W05/W08 | AST 变化、权重变化、三组对照与独立验收 |
| 检查点、暂停/恢复与历史 | W01/W06/W12 | 杀进程后恢复、预算连续、半成品不可见、失效不可恢复 |
| 历史比较与选择 | W07/W11 | 同协议重评、硬门槛与版本冲突、人工选择准确历史快照 |
| 分裂与真实多路并行 | W06/W09 | 独立分支、真实进程重叠、故障隔离和统一预算 |
| 群体交流与多样性 | W09/W14/W16 | 专长保留、合法经验迁移、无验收答案泄漏 |
| 内部模块化与协同演化 | W13/W16 | 共享参数约束、语义兼容、组合后整体验证 |
| 向量化与语言化经验 | W14 | 空间版本、结构/行为/语义索引及新任务复用测量 |
| 环境反馈、演示、自监督 | W02/W15 | 真实更新及留出行为，不以代理损失替代任务验收 |
| 发布、回滚、权限和数据安全 | W01/W04/W07/W10/W12 | 越权拒绝、进程隔离、原子切换、撤回和恢复失败路径 |
| 可观察性与交付可复现 | W08/W10/W11/W17 | 运行事件、来源记录、CLI 输出、截图与制品摘要 |

## 11. 执行门槛与风险处置

- P1 可在无 GPU、无外部教师凭据时实施；P2 必须有真实 CPU 张量后端，不以 mock 验收。
- P3 并行允许 CPU 多进程，不能以没有多 GPU 为理由只交付单 worker；资源不足时缩小验收模型而不是删除并发合同。
- 模型质量达不到 W00 门槛时分析数据/表达能力/优化预算，使用训练和验证集改进；独立验收集不能成为调参接口。
- 上游调用许可、真实业务数据和业务门槛是业务验证前提；缺失时明确列为未验证，不阻断可自行验证的工程交付。
- 性能优化依测量推进；不预先引入分布式队列、向量数据库、通用张量差分或复杂前端框架。
- 学习器自我改写与自动晋升属于后续独立授权范围；本计划完成策略/提议器/recipe 版本化与人工受控更新，不将它描述成已经实现的元学习。
- 每个阶段结束更新实际状态与证据，不重复维护另一份架构，也不把本计划中的复选框当作测试通过证据。

## 12. 执行状态

本节随实施更新，记录实际已执行并验证的工作包；勾选框以实际运行结果为准。

| 工作包 | 状态 | 验证 |
|---|---|---|
| W00 验收合同 | ✅ | `python examples/self-learning/generate.py --self-check`（分组泄漏 0、缺失模态 abstain、holdout 不存标签字节）；`learn-demo.zio` 回归保持 |
| W01 宿主存储与制品身份 | ✅ | `cargo test -p grove --test store_contract`（22，含 SIGKILL 写进程后半成品不可见） |
| W02 反馈生命周期 | ✅ | `cargo test -p grove --test feedback_contract`（12） |
| W03 教师接口 | ✅ | `cargo test -p zio-ai --test teacher_contract --features http`（16）+ `grove --test teacher_local` |
| W04 worker 与隔离 | ✅ | `cargo test -p grove --test worker_contract`（10，命名空间网络阻断实测）+ worker 协议测试（13） |
| W05 联合学习 driver | ✅ | `cargo test -p grove --test dual_learning_contract`（9）；W00 任务实测线性基线 0.531 → 非线性候选 1.000（+46.9pp） |
| W06 检查点/暂停/恢复/分裂 | ✅ | `cargo test -p grove --test checkpoint_contract`（10，含杀进程续训与不中断运行逐字节一致） |
| W07 历史重评与发布资格 | ✅ | `cargo test -p grove --test evaluation_contract`（11） |
| W08 库调用与本地端到端 | ✅ | `cargo test -p grove-app`（6）；`grove demo --case dual` 实测发布 v1 |
| W09 多 worker 群体协调 | ✅ | `cargo test -p grove --test population_contract`（8，两 worker 窗口重叠实测 >500ms）；`grove demo --case population --workers 2` |
| W10 产品 API 与授权发布 | ✅ | `cargo test -p grove-app --features http --test api_contract`（6）；`grove serve` 真实进程 |
| W11 产品界面 | ✅ | `cargo test -p grove-app --features http --test ui_contract`（5）+ 浏览器实测走通「观察 → 真实 worker 预测 → 纠正 → 比较 → 发布」；预测由隔离 torch worker 真实产生（`class 0, snapshot e4cc7052fdd6`），纠正显示 accepted/in force/**尚未学习**；过期发布版本被拒（`conflict: publication is at version 1, caller expected 99`）。截图取证因浏览器宿主在本会话中失联未完成，交互证据以 DOM 断言与真实 HTTP 响应为准 |
| W12 撤回、保留与恢复 | ✅ | `cargo test -p grove --test lifecycle_contract`（10）：GC 在 7 天保留期下删除 0 个、置 0 后删除 4 个不可达制品并拒绝 4 个可达制品；撤回信号使相关快照不可发布（`incompatible-state`）而无关快照仍可发布 v1；状态制品被删/被改字节均返回 `artifact-unavailable`；重启后过期租约被回收、旧 epoch 回执被拒（`conflict`） |
| W13 模块组合与联合微调 | ✅ | `cargo test -p grove --test composition_contract`（9）：同维不同语义空间被拒（`protocol-violation`）、共享参数不可为两个版本、组合不得弱化权限闭包、局部替换产生新身份且不改动原分支、联合计划强制整体评估；`grove demo --case modular` 真实演化并组合，异构空间组合被拒并给出原因，合成体联合训练 val accuracy 1.000 并以该整体分数发布 v1 |
| W14 三索引经验与复用 | ✅ | `cargo test -p grove --test memory_contract`（12，含四项硬拒绝）+ `cargo test -p zio-cli --test lib_contract`（10，含 28+23 标记）。**复用测量结果为负且如实记录**：未参与发现的任务上，总描述长度 18 → 22（一次调用改两次），抽象行为正确但在 3 叶规模下不划算 |
| W15 扩展 recipe | ✅ | `python -m unittest discover -s workers/torch/tests`（72，OK）；`cargo test -p grove --test recipe_contract`（15）。实测：软蒸馏 val KL 0.6918→0.1264；偏好排序 0.5469→0.9531 且弃答不进目标（加/不加 12 组弃答损失完全相同）；演示 0.9297，25% 非法动作被 mask 后 0.8932；自监督表征移动（cosine 0.7700）而任务指标单列 0.5117 仍不过门槛；延迟奖励 64/64 结算、未结算时参数变化恰为 0.0；策略梯度回报 −1.0000→0.7500。**外部供应商未提供软输出，仅本地教师的硬蒸馏已联调** |
| W16 专家集成与多教师蒸馏 | ✅ | `cargo test -p grove --test ensemble_contract`（12）：异构输出空间拒绝绑定、无可用专家 abstain、投票平局无胜者、`all-agree` 缺一专家即 abstain、单次调用按全部专家计费且受每调用预算约束、群体一致只作为带一致度的蒸馏目标；`demo --case modular` 现场对比 vote（存活专家可答）与 all-agree（同样输入 abstain） |
| W17 全链路交付 | ✅ | `cargo test --workspace --all-features` **364 通过 / 0 失败**；`python -m unittest discover -s workers/torch/tests` **72 通过**；三个实际场景全部可运行：dual（0.539→0.996，发布真实训练权重 v1）、population（两分支并行窗口重叠 966ms，账本 1000/1000，僵尸回执被拒）、modular（模块分离演化→组合→整体 1.000 发布 v1） |

已执行部分证明的是工程闭环（真实存储、真实信号生命周期、真实教师调用、真实张量训练、真实隔离与并发、真实 UI 交互与真实发布），不含任何业务收益声明。W00 的门槛是待达成门槛的冻结值，其达标情况以 W05/W08 的实测为准。

**未验证的外部环境**：商业上游模型的软输出与授权条款、真实业务数据与业务门槛、GPU 推理成本、截图证据（浏览器宿主在本会话失联）。以上一律不因本地闭环通过而标记为已联调。
