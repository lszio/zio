# Zio 程序合成模块计划 —— 学习型提议 × eval 裁判 × 语言化记忆

> Version 0.2 — 规划文档(全部为计划,不代表已实现;实现状态以
> [特性矩阵](feature-matrix.md)为准)
>
> 本文档定义一个独立演进的模块:**程序合成(自学习)模块**——在现有
> 同象性学习器 MVP 之上,以可插拔的学习机器(LLM 为主线,遗传算子、
> RL、神经网络各有明确扩展位)为提议器、语言化记忆为经验、zio eval
> 为裁判,完成"程序改写自身"的闭环。包含论文与汇报的交付规划。
> v0.1 → v0.2 修订见[第 9 节](#9-修订记录)。

---

## 1 定位与核心抽象

### 1.1 一个循环,N 种提议器(SICP 视角)

学习循环是 SICP 4.3 非确定性求值器(`amb`)的确定性工程化。`amb`
求值器的三个构件——候选来源、`require` 过滤、回溯 driver——在本模块
各有一个对应物:

| SICP 4.3 `amb` 求值器 | 本模块 |
|---|---|
| `(amb e1 e2 …)` 提供候选 | **提议器**(proposer):`(proposer task history) → 候选文本列表` |
| `(require p)` 过滤 | **两道闸门**(解析、闭世界白名单)+ **评分**(eval 损失;零损失即 require 满足) |
| driver loop 失败回溯、换下一个候选 | **代际循环**:预算(`:max-generations` / `:max-evals`)代替无限回溯 |
| 深度优先的固定枚举顺序 | **枚举提议器**(默认,即朴素 amb)——现有 MVP |
| — | **LLM 提议器**(学习型 amb:看着上一代损失地形选下一批) |
| — | 更多学习机器(遗传算子、RL 策略、神经网络)= 新的提议器实现,**循环不变** |

结论:**枚举器、LLM、遗传算子、RL 策略都是同一抽象(proposer)的
实现,学习循环只有一份**。混合提议器 = 多路候选在每代做 merge
(SICP 3.5 流的有限代快照;不引入 delay/force,每代是有限批)。

eval 裁判是 SICP 4.1 元循环求值器的自指延伸:语言用自己的 eval 评判
用自己写成的程序——同像性使候选无需任何桥接即可进入裁判。

### 1.2 角色分工与扩展位

| 能力 | 角色 | 强项 | 短板(由闸门 + 裁判兜底) |
|------|------|------|--------------------------|
| zio eval | **裁判**(ground truth) | 确定性、可验证、可打分 | 盲目——枚举搜索空间组合爆炸 |
| 学习机器(LLM / 遗传算子 / RL 策略 / 神经网络…)| **提议器** | 从样本/描述直接产生候选 | 幻觉、随机、不可信 |
| Embedding | **记忆的语义桥**(可选索引,L4) | 跨模态/自然语言任务的相似检索 | 不生成、不执行;仅当任务以 NL 到达时启用;**不用于候选去重** |

两条教义,先于任何技术选型:

- **唯一裁判教义**:eval 是唯一 ground truth。任何学习型组件(损失
  预测器、先验模型、神经排序器)只能对候选**重排/剪枝以节省 eval
  预算**,不得替代 eval 判定成功——建议性,非权威。
- **RL 环境视角**:学习循环本身就是一个 RL 环境——state =
  (task, history),action = 候选批,reward = −损失。经验库
  (memory.zio)记录的 (任务, 程序, 得分) 就是轨迹数据;任何 RL
  学习器、以及元层面的 bandit(在提议器/温度/算子之间调度)都只是
  这份数据流的**消费者**,不需要改动循环。

各技术的扩展位(落地层与阶段,防止泛化变成范围失控):

| 技术 | 角色 | 落地层 | 阶段 |
|------|------|--------|------|
| 枚举搜索 | 提议器(默认) | 纯 Zio,无宿主 | L3(现有 MVP 降级复用) |
| 遗传算子(变异/交叉,`:seed` 驱动) | 提议器 | 纯 Zio——随机性在提议边界,符合确定性合同 | L3 可选(天然 baseline) |
| LLM | 提议器 | zio-ai `LlmHost` | L1–L3 **主线** |
| Bandit 元调度(提议器选择、参数调度) | 元提议器 | 纯 Zio,消费经验数据 | L4 可选 |
| 神经网络(玩具前向) | "模型即数据"演示 | 纯 Zio(权重是 zio 数据,前向可 `eval`/`print` 检视) | L4 可选 |
| 神经网络(真实推理) | 提议器 / 学习型先验 | `ModelHost`(ONNX 类运行时) | 远期,不在 L1–L4 |
| Embedding / 向量化 | 记忆语义桥(可选) | zio-ai `EmbedHost` + vector.zio | 协议 L1 主线;检索用途 L4 可选(仅 NL 任务) |

### 1.3 记忆的 Lisp 本性(向量化之后)

向量是外部语义世界的通用桥,但不是 Lisp 的母语。按照语言自身特性,
记忆可以完全是符号与行为的一等公民,口号是:**检索即求值,泛化即
定义**。

1. **行为指纹(解释器即 embedder)**:程序在一组规范输入电池上的
   输出向量,由 eval 确定性计算。它是对"语义相似"最诚实的度量——
   不经任何外部模型压缩,直接比较可观察行为。文本 embedding 对短
   符号表达式几乎无信号,行为指纹正相反。
2. **结构索引(代码即索引)**:规范化 AST(L3 已有)+ 子表达式
   共享——相同的子结构物理共享,检索命中即复用;alist/哈希这些
   Lisp 原生结构就是关联记忆的形态。
3. **反统一蒸馏(泛化即定义)**:对聚类中的成功程序求**最小泛化**
   (Plotkin LGG / anti-unification),把公共模式提升为 `defmacro`
   候选——这是"泛化"的符号算法,天然产出宿主语言的宏,无需黑箱。
   LGG 模式含洞;不定长重复(ellipsis)模式为远期。
4. **环境吸收(记忆即语言)**:晋升的宏定义进经验模块;下一次学习
   `(require :learn.experience)` 后,`:ops` 词汇表与白名单同步扩充
   为 基础词汇 ∪ 经验模块导出。SICP 环境模型(3.2)说:求值的意义
   由环境决定——我们让学习去扩充环境。**记忆的终态不是数据库,是
   语言本身**;经验文件本身是可 load 的 zio 源码(数据字面量 + 定义)。

向量的位置:vector.zio 仍是通用库,但在记忆中降为**可选语义桥**——
只有任务以自然语言/外部描述到达时才需要;数值与符号任务用指纹与
结构已足够。度量随之升级:除调用递减曲线外,新增 **DSL 收缩**(环境
吸收后,求解程序的描述长度随经验递减)——"语言变聪明了"的直接
证据,与 DreamCoder 的 MDL 指标对齐,但实现在宿主语言层。

### 1.4 为什么 S 表达式是提议的正确目标语言

1. **解析即校验**:畸形输出在 eval 之前被 reader 确定性拒绝,可自动重试;
2. 语法正则、树结构对 token 序列友好(对遗传算子则是天然的变异/交叉底物);
3. 学习器的 `:ops` / `:constants` / `:max-depth` 约束直接映射为
   提示词约束 + 解析后 AST 闭世界白名单,双层护栏;
4. 提议器是不可信外部进程,与 ProcessHost"不用 shell、参数白名单"
   同一安全等级——**包含由构造保证**:候选永远只过解析 → 白名单
   **两道闸门**,再进入唯一裁判 eval。校验逻辑在循环内,对所有提议器
   一致生效;eval 是评分权威,不承担安全闸职能。

### 1.5 与项目论点的关系

README 定义 zio = "同像性 + eval/apply" 的 Lisp。本模块把 AI 组件全部
纳入同像性域:**模型是数据(响应文本、权重向量),提示是数据(S 表达
式模板),记忆是数据(可 load 的经验模块),学习是一次可被 `eval` /
`macroexpand` / `print` 检视的数据变换循环。** 终态即落地页口号
"Programs that rewrite themselves"的可 demo 闭环。

当前 MVP 学习器(`lib/zio/learn.zio`)卡在枚举:beam 24、深度 2 约需
5 秒(AST 解释器),深度 3 不可行(量化判据见 5.4)。

---

## 2 模块边界与命名

### 2.1 布局

与 core 的边界遵循 ADR-009(核心最小):宿主 AI 能力协议是 ADR-009
预留的 "LLM API / 自学习" 能力面,进新 crate `zio-ai`;学习逻辑全部是
纯 Zio 库。**core 零改动。**

```text
ai/                        # crate zio-ai:宿主 AI 能力协议(能力插座,不含算法)
├── src/lib.rs             # LlmHost / EmbedHost trait + install(外部 attach)
├── src/mock.rs            # Mock:record / replay 两种模式
└── src/http.rs            # OpenAI 兼容实现(feature = "http")
lib/zio/learn.zio          # 学习循环:driver + 白名单 + 预算 + 评分(已存在,泛化)
lib/zio/proposer.zio       # [L2] 提示模板 + 响应解析 + 重试 + make-llm-proposer(/ make-gp-proposer)
lib/zio/vector.zio         # [L4] 通用余弦向量库(纯 Zio)
lib/zio/memory.zio         # [L4] 经验库:三索引检索 + 反统一蒸馏 + 环境吸收
```

- **注入方式 = 外部 attach**:`zio_ai::install(ctx, llm, embed)` 用
  NativeFn 闭包把 `llm-complete` / `embed` 注册进 env。host 以
  `Option<Arc<dyn LlmHost>>` 传入:绑定总是注册,宿主缺失时调用返回
  稳定前缀错误 `capability-denied: …`(合同测试锚定该前缀;真正的
  类型化错误变体若未来需要,作为独立 core 提案)。不采用"IoHost 同构
  的 EvalContext 字段"方案——那要求 trait 进 core,违反 ADR-009;
  协议独立成 crate 的前提就是注入点外置。
- **crate 依赖方向**:zio-ai 依赖 zio-core(取 EvalContext/env),
  core 不依赖 zio-ai。CLI 以 `--llm mock:recordings.json` 等方式装配。
- **协议只收能力,不收算法**:zio-ai 内没有任何学习算法——算法要么
  是纯 Zio 提议器(枚举/遗传/bandit),要么运行在宿主进程(LLM API、
  未来的 ModelHost 推理)。crate 是能力插座,不是智能本体。
- **`ModelHost` 为命名预留的远期扩展位**(ONNX 类张量推理,服务神经
  提议器/学习型先验);trait 形状待真实需求出现再定,现在不实现。
- LLM/embed 的 Rust 数学加速(原生 HNSW 索引等)为远期项,进
  NumericKernel 相邻层,不进 core;
- 工作区从 2 个 crate 变为 3 个时,`tools/project-status.sh` 的
  "Workspace crates" 计数需同步(现硬编码为 2)。

### 2.2 命名表(v0.1 → v0.2)

| 旧名 | 新名 | 理由 |
|------|------|------|
| crate `zio-cognitive` | **`zio-ai`** | ADR-009 预留名;按域命名的宿主能力协议,能力各自有精确命名的 trait(`LlmHost` / `EmbedHost` / 远期 `ModelHost`)。不叫 zio-llm 因为它不止 LLM,不叫 zio-cognitive 因为"认知"是修辞 |
| `docs/cognitive-plan.md` | **`docs/synthesis-plan.md`** | 模块的研究域是程序合成,与论文题目 Homoiconic Program Synthesis 对齐 |
| `lib/zio/llm.zio` | **`lib/zio/proposer.zio`** | 按角色命名(SICP 传统);LLM 提议器与遗传提议器都是"提议器",同住一库 |
| `lib/zio/embed.zio` | **`lib/zio/vector.zio`** | 它是通用余弦向量库(结构命名),embedding 只是数据来源之一 |
| `examples/cognitive-demo.zio` | **`examples/synthesis-demo.zio`** | 同上 |
| "三道闸" | **"两道闸门 + 一个裁判"** | eval 执行候选而非拒绝候选,是评分权威不是安全闸 |
| 构造器命名 | **`make-*`**(SICP 惯例) | `make-llm-proposer` / `make-gp-proposer` / `make-enum-proposer` / `make-vector-db` / `make-memory` |
| `LlmHost` / `EmbedHost` / `llm-complete` / `embed` / `:proposer` | 不变 | 业界通用或已是房屋风格 |

---

## 3 分阶段计划

### L1 宿主协议 zio-ai(3-5 天)

| 任务 | 说明 |
|------|------|
| `LlmHost` trait | `complete(prompt, opts) -> Result<String, HostError>`;opts 含 temperature / max-tokens / stop |
| `EmbedHost` trait | `embed(texts) -> Result<Vec<Vec<f64>>, HostError>` |
| MockLlmHost / MockEmbedHost | **record / replay 双模式**:record 经真实调用落盘;replay(默认)按录制脚本回放,回放键 = hash(prompt),**miss 即 fail-fast**(合同测试不得触网) |
| HTTP 实现(feature = "http") | OpenAI 兼容 chat/embeddings 端点;超时与响应大小上限(对齐 ProcessHost 纪律) |
| `install`(外部 attach) | 注册 `llm-complete` / `embed`;core 零改动;无宿主 → `capability-denied:` 前缀错误 |

**验收**:合同测试全部走 Mock、不触网;replay miss 的 fail-fast 有
测试;HTTP 实现有超时测试;无宿主调用返回锚定前缀错误;status 脚本
crate 计数更新(ADR-016 已在案,本阶段交付其协议实现)。

### L2 提议器库 proposer.zio(约 1 周)

| 任务 | 说明 |
|------|------|
| 提示模板 | S 表达式模板:任务(样本/ops/常量/深度)+ 上一代回喂渲染进提示 |
| 响应解析 | 一个 completion → 多个候选文本;解析即校验(畸形候选在此淘汰,带原因) |
| 确定性重试 | 解析失败 → 带失败原因的修正提示,重试 N 次;重试序列确定 |
| `make-llm-proposer` | 实现 proposer 协议(数据形状见 L3),经 `llm-complete` 调用 |
| 确定性合同 | 同 prompt + 同 Mock 回放 → 同输出;纯 Zio 解析与 Rust 参考实现对拍 |

**验收**:Mock 下 `(make-llm-proposer …)` 对固定 task 端到端产出稳定
候选列表;全部合同测试不触网。

### L3 学习循环泛化(1-2 周)——本模块的关键阶段

| 任务 | 说明 |
|------|------|
| proposer 协议 | 见下方代码块;`learn-function` config 增加可选 `:proposer`,缺省 = `make-enum-proposer`(现有枚举器降级改名,向后兼容) |
| 循环重构 | 深度有界 → **代际 + 预算有界**:`:max-generations` / `:max-evals` 成为 config 一等公民 |
| **校验上移进循环** | `learn--allowed?` 闭世界 allowlist:候选 AST 中每个符号 ∈ {x} ∪ constants ∪ ops,深度与节点数上限;**对所有提议器一致**,解析失败或非法 → 淘汰并回喂修正信息 |
| **候选级错误隔离** | 评分用 `(try (eval …) (catch any 最差分))`:除零 / arity 错 / 非数值记最差分,不终止学习运行(try/catch 已实现) |
| 结果回喂 | 上一代 (程序, 得分) 列表拼入下一代提示;得分以归一化相对误差展示(学习机器可读),内部仍用整分排序 |
| AST 规范化去重 | 交换律排序 + 单位元消解 → 结构哈希;字符串相等保留为兜底;**embedding 不用于候选去重** |
| 遗传提议器(可选) | `make-gp-proposer`:AST 变异/交叉,`:seed` 驱动(learn.zio 已保留该键);零宿主、确定性可回放,是 LLM 提议器的天然对照 baseline |

```lisp
;; proposer 协议(learn.zio 定义,任何提议器实现同一形状):
;; (proposer task history) -> 候选字符串列表
;;   task    = {:samples … :ops … :constants … :max-depth …}
;;   history = ({:generation n :candidates (…)} …)   ; 上一代回喂
;; 提议器只产文本;解析、白名单、去重、评分、选择、预算全部在循环内。
```

**验收**:[[1 3] [2 5] [3 7]] → 2x+1 在 Mock 提议下通过现有合同;
枚举 / LLM / 混合提议器可切换(做了遗传提议器则一并纳入);深度 3
任务在 LLM 提议下可解,且基准中"枚举不可行"有量化定义(见 5.4);
预算指标(`:max-generations` / `:max-evals`)可测。

### L4 经验闭环:记忆的语言化(2-3 周)

| 任务 | 说明 |
|------|------|
| 经验库 `lib/zio/memory.zio`(主线) | `make-memory` / `memory-remember!` / `memory-recall`:(任务, 程序, 得分) 三元组——**这就是 RL 轨迹数据**。检索走**三索引**:① 结构索引(L3 规范化 AST + 子表达式共享);② **行为指纹**——程序在规范输入电池上的输出向量,由 eval 确定性计算(解释器即 embedder);③ 向量索引(可选,仅 NL 任务)。相似度 = 多索引加权;命中历史做 few-shot 注入 |
| **反统一蒸馏**(主线) | 对聚类成功程序求**最小泛化**(Plotkin LGG / anti-unification)——泛化的符号算法,公共模式直接提升为 `defmacro` 候选;行为描述子 = 指纹/输出向量(MAP-Elites 标准);蒸馏候选必须通过既有合同测试 |
| **环境吸收**(主线) | 晋升的宏定义进经验模块(`memory/experience.zio`——**本身是可 load 的 zio 源码**);后续学习 `(require :learn.experience)` 后 `:ops` 词汇表与白名单同步扩充(基础词汇 ∪ 经验模块导出)。记忆的终态不是数据库,是语言本身 |
| `lib/zio/vector.zio`(可选语义桥) | 通用余弦向量库(独立可用):`make-vector-db` / `vector-insert!` / `vector-search` / `vector-save` / `vector-load`(json 持久化,带模型版本字段);sqrt 用纯 Zio 牛顿迭代(core 无 sqrt/abs,是否加内建另行决策);对拍 Rust 参考实现;规模 ≤ 10³ 条 |
| 自修复 | eval 报错(含行列号,ADR-004)回喂提议器修复候选 |
| Bandit 元调度(可选) | 消费轨迹数据,在提议器/参数之间调度(1.2 RL 环境视角的元层实例) |
| 玩具神经网络(可选) | 纯 Zio 前向传播(权重是 zio 数据)——"模型即数据"的同像性演示,不作训练目标 |

**验收**:同一任务重复求解时 LLM 调用次数下降(记忆生效的可测证据);
反统一蒸馏产出至少一个从经验提炼的真实宏;**DSL 收缩报表**——环境
吸收后,求解程序的描述长度随经验递减("语言变聪明了"的直接证据)。

---

## 4 既有决策的约束(全部为保护)

- **ADR-014(单线程)**:阻塞式 LLM 调用会卡住 eval 循环——对离线学习
  脚本可接受(文档如实标注);agent 模式等 VM 并发。**WASM 不在
  L1-L4 承诺内**:同步 WASM 栈无法等待 fetch(需 Asyncify 或 worker
  桥),JS 宿主注入留作未决项;WASM 侧演示仅 Mock 回放。
- **确定性合同**:核心循环(解析/校验/评分/选择)对同样提议永远产出
  同样结果;随机性被隔离在提议边界(含 GP 的 `:seed`、LLM 的
  temperature 在 Mock 回放中冻结),Mock 回放保证科研可复现。
- **ADR-012(并发原语诚实化)**:LLM 等待不伪装成异步;同步阻塞
  如实标注。
- **可观察性**:每次 LLM 调用作为数据记录(prompt/completion/成本/
  延迟),对齐设计文档 Observer 的 `LearningGenerationScored` 事件;
  轨迹记录(1.2 RL 视角)复用同一记录层。

---

## 5 论文规划

### 5.1 定位与题目候选

方向:**学习机器提议 + 确定性求值裁判 + 同像性经验记忆**的程序合成
系统。候选题目:

1. *Homoiconic Program Synthesis: S-Expressions as a Verifiable Search
   Space for Learned Proposers*
2. *Code as Data as Experience: Deterministic Replay Contracts for
   Self-Improving Program Synthesis*

### 5.2 贡献声明(草拟)

1. **两道确定性闸门 + 唯一评分权威**:在同像性基底上,不可信提议器的
   可靠性问题被转化为 reader 解析 + 闭世界 AST 白名单两道确定性检查;
   eval 不是闸门,是共享的确定性裁判(评分即语义 require)。校验在
   循环内对所有提议器一致生效,随机性被限制在提议边界;
2. **确定性回放合同(reproducibility engineering)**:Mock 提议器使
   整个学习循环可精确复现——对学习型系统常见的不可复现性给出工程
   解法;定位为系统/工程贡献,不冒充科学发现;
3. **记忆即语言(与 DreamCoder library learning 正面对比)**:记忆以
   宿主语言形式存在——行为指纹(解释器即 embedder)、结构索引、
   反统一蒸馏(Plotkin LGG → `defmacro`)、环境吸收(`require` 经验
   模块);检索即求值、泛化即定义。与 DreamCoder 同源(library
   learning),差异必须写透:抽象以**宿主宏系统**表达、经
   `macroexpand` 可检视、经验文件本身是可执行数据,而非内部 DSL;
   学习指标为 **DSL 收缩**(求解程序描述长度递减,对齐其 MDL);
   全程不训练权重;
4. **提议器抽象的可替换性**:同一循环下枚举 / LLM / 遗传提议器可切换
   对比(消融即证据);RL 环境视角说明经验数据可直接喂给学习器——
   架构对"下一个学习机器"是开放的。

### 5.3 相关工作(须诚实对照)

- **DreamCoder**(wake/sleep 程序合成):最近亲缘,宏蒸馏 ≈ 其
  library learning;差异表述见 5.2 贡献 3;
- **反统一 / LGG**(Plotkin):泛化的符号算法——反统一蒸馏的理论
  来源(替代聚类黑箱);
- **LLM-SR**(LLM 符号回归)、**FunSearch**(LLM + 评估器孤岛)、
  **Eureka**(奖励设计)——差异点:同像性持久化、回放合同、宏以宿主
  宏系统表达;
- **神经/RL 程序合成**(policy-gradient 合成、neural guided search):
  提议器谱系的延伸——本计划以架构扩展位收纳(1.2),实验主线仍是
  枚举 vs LLM vs 混合,策略网络列为 future work(ModelHost);
- **PySR / 遗传规划**:枚举式符号回归——即本模块的默认提议器,作为
  baseline;遗传算子提议器是其库内变体;
- **MAP-Elites**:质量-多样性搜索——行为指纹即其**行为描述子**在
  宿主中的实现(样本输出向量),非文本 embedding;
- **RAG**:检索增强——经验检索为其在程序合成域的特化;
- **SICP**(Abelson & Sussman):学习循环 = amb 求值器的确定性工程化
  (1.1),eval 裁判 = 元循环求值器的自指——基底语言选择的理论依据。

### 5.4 实验设计

- **基准**:符号回归标准套件(Nguyen、Keijzer、PF、Constant 系列)+
  设计文档样例;基线 = 纯枚举 MVP(PySR 参照),遗传提议器为第二基线;
- **"枚举不可行"量化定义**:仅当候选空间下界 ≥ 10⁶(或实测枚举时间
  超预算一个数量级)的任务计入"枚举不可行"——防止 baseline 放水的
  审稿质疑;
- **消融**:提议器 ∈ {枚举, LLM-only, 混合(做了 GP 则并入)} ×
  记忆 ∈ {开, 关};
- **指标**:solve rate@budget、每解 token 成本、eval 次数、每代闸门
  通过率(合法候选率)、跨任务调用递减曲线(记忆生效证据)、复现率
  (回放验证);
- **可复现声明**:全部实验可由 Mock 回放精确重演。

### 5.5 投稿路径

1. **工作坊论文**(L3 完成、基准跑通后):LLM for Code / 程序合成方向
   的 NeurIPS/ICLR workshop,或 GECCO(GP 谱系);
2. **完整论文**(L4 完成后):系统方向投 Onward!(同像性论点契合
   "远见"类论文)或 arXiv 预印本 + 社区传播。

---

## 6 汇报规划

### 6.1 内部里程碑汇报(每阶段收尾,约 15 分钟)

- L1:协议 + Mock 演示(无网环境全流程,含 record→replay 闭环);
- L2:proposer.zio demo(固定 task 端到端稳定产出,持久化文件即数据);
- L3:**关键汇报**——Mock 回放下深度 3 任务求解(枚举不可行 → 学习型
  提议可解),多提议器对比数据;
- L4:闭环 demo(记忆使调用次数下降 + 第一个蒸馏宏亮相 + 经验模块
  load 即召回 + DSL 收缩数字)。

### 6.2 外部技术分享(20-25 分钟,约 12 页)

1. 问题:程序合成的枚举瓶颈(用 MVP 的 5 秒/深度 2 数据开场);
2. 三角色架构图(eval 裁判 / 学习机器提议 / 语言化记忆)+ amb 视角
   (一个循环,N 种提议器)+ RL 环境视角一页;
3. 为什么是 S 表达式:解析即校验(现场演示:畸形候选被 reader 拒绝);
4. 安全漏斗:两道闸门 + 一个裁判(闭世界白名单;能力词汇表);
5. 确定性合同:Mock 回放(现场重放同一次学习);
6. 学习器现状:design 样例 + 平方数据集(现有 learn-demo);
7. 结果回喂:损失地形作为数据;
8. 记忆的语言化:结构索引 + 行为指纹(解释器即 embedder)+ 调用递减曲线;
9. 反统一蒸馏与环境吸收:数据变回语言的代码(现场 require 经验模块;
   与 DreamCoder 的差异一页);
10. 与 LLM-SR/FunSearch/DreamCoder 的差异表;
11. 基准结果与消融;
12. 路线图与开源仓库(扩展位表:遗传/RL/神经各有位置)。

### 6.3 演示脚本(冻结版)

`examples/learn-demo.zio`(现有)→ L3 后追加 `examples/synthesis-demo.zio`:
Mock 回放深度 3 求解 → 真实 API(可选开关)→ 检索命中历史 → 蒸馏宏展示。
演示全程可离线重放(Mock),现场网络故障不构成风险。

---

## 7 里程碑总表

| 里程碑 | 内容 | 交付物 | 论文/汇报节点 |
|--------|------|--------|---------------|
| M1(L1) | zio-ai 宿主协议 + Mock | 新 crate + 合同测试 | — |
| M2(L2) | proposer.zio 提议器库 | 提议器端到端 demo + 对拍报告 | 论文 v0 提纲;内部汇报 |
| M3(L3) | 学习循环泛化 | 深度 3 求解 demo | 工作坊论文投稿;外部分享首秀 |
| M4(L4) | 经验闭环 + 宏蒸馏 | 调用递减曲线 + 蒸馏宏 | 完整论文 + Onward!/arXiv |

---

## 8 风险

| 风险 | 缓解 |
|------|------|
| LLM 成本失控 | 调用全量记账;Mock 主导开发;缓存复用结构相同候选 |
| 不可信提议注入危险代码 | **闭世界 allowlist 为主**(符号集封闭 + AST 节点数上限;deny 列表只能作兜底——spit 已注册是教训);校验在循环内对所有提议器生效 |
| 候选 eval 崩溃终止学习 | try/catch 候选级错误隔离为强制实现(L3) |
| 随机性破坏可复现 | 回放合同是合同测试的一部分,非可选项;replay miss 即 fail-fast;GP/LLM 随机性冻结在提议边界 |
| 泛化失控(什么都想做) | 扩展位表(1.2)是范围合同:L1-L4 主线只有 LLM 与 embedding;遗传/bandit/玩具 NN 标"可选",ModelHost 只留名不实现 |
| embedding 维度/成本 | 语义桥为可选,主线(结构 + 指纹)不依赖外部模型;MVP 固定小模型;向量库格式带模型版本字段 |
| 纯 Zio 数值循环性能 | 向量库规模上限 10³ 条并声明;学习循环受 eval 预算约束;Rust 加速为远期项 |
| 蒸馏出错误宏 | 蒸馏候选必须通过既有合同测试才能晋升 |

---

## 9 修订记录

**v0.1 → v0.2(2026-09,评审后重构)**

1. 宿主协议从"经 EvalEngine 注入的宿主路由 + trait 进新 crate"(依赖
   方向自相矛盾:EvalContext 在 core,无法持有依赖方 crate 的 trait)
   改为**外部 attach**:`zio_ai::install` 注册绑定,core 零改动;
2. 解析/白名单职责从 LLM 层上移到学习循环——白名单是任务属性,必须
   对所有提议器一致生效;"三道闸"改为"两道闸门 + 一个裁判";
3. 新增 L3 硬需求:**候选级错误隔离**(try/catch)与**预算一等公民**
   (`:max-generations` / `:max-evals`,solve rate@budget 的前提);
4. L2/L3 重排:先提议器(最关键假设)后 embedding(价值最不确定);
   embed 降为 L4 的 vector.zio + memory.zio;
5. 候选去重从 embedding 改为 **AST 规范化 + 结构哈希**;embedding 只做
   任务级经验检索;新颖性/聚类用行为描述子(MAP-Elites);
6. **泛化提议器抽象,不绑定 LLM**:遗传算子(L3 可选)、bandit 元调度
   与玩具 NN(L4 可选)、ModelHost(远期,只留名)各有明确扩展位;
   立两条教义——唯一裁判(学习型组件只能重排,不得替代 eval)与
   RL 环境视角(经验即轨迹数据,学习器只是消费者);
7. 论文措辞:回放合同定位为 reproducibility engineering;宏蒸馏与
   DreamCoder library learning 正面对比;"枚举不可行"量化定义
   (候选空间下界 ≥ 10⁶);
8. 命名调整见 2.2 命名表;SICP amb 视角统一循环抽象(1.1);
9. WASM/fetch 从"天然出口"降级为未决项,不在 L1-L4 承诺内;
10. **记忆的语言化(向量化之后)**:memory.zio 重设计为三索引经验库
    (结构、行为指纹、可选向量);"解释器即 embedder";宏蒸馏升级为
    反统一蒸馏(Plotkin LGG)+ 环境吸收(require 经验模块,`:ops`
    词汇扩充);新增 DSL 收缩指标;embedding 降为可选语义桥。
