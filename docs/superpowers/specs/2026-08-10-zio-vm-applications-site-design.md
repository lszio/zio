# Zio VM、应用场景与落地页设计规格

**日期：** 2026-08-10

**状态：** 已批准

**范围：** 运行时重构、性能工程、四个应用场景、文档同步、双语静态落地页

## 1. 背景与决策

Zio 当前已经拥有可工作的 Reader、S-expression、AST Eval、闭包、宏、尾调用、模块原型和 ZOS 原型，但代码、文档、示例与公开能力描述没有同步：单元测试通过，官方端到端示例仍有失败；README 和架构文档中的测试数、内置函数数、特殊形式数和组件状态已经落后；Datalog 与 persistent 库仍是接口草案。

本轮工作不继续在 AST Eval 和 `builtins.rs` 上堆叠功能。用户选择先建设编译器和执行引擎：

```text
Expanded Sexp → ZIR → Optimize → Bytecode → VM
```

AST Eval 保留为语义参考实现。第一阶段实现字节码 VM，不直接接入 Cranelift、LLVM 或其他 JIT；当 VM、四个应用和性能基准稳定后，再单独评估 JIT。

## 2. 目标

1. 建立结构化 ZIR、字节码编译器和可嵌入 VM。
2. 保留 AST Eval，通过差分测试保证两个执行引擎的语义一致。
3. 用基准数据定位热点，减少重复解析、名称查找、Value 克隆和临时分配；不预设脱离证据的性能倍数。
4. 通过显式 Capability 协议隔离模块、I/O、进程、数值计算和运行时观测。
5. 实现四个可执行场景：Pacman 待更新检查、科学计算、数据流水线 DSL、基于同像性的符号学习器。
6. 更新 README、架构、路线图、ADR、教程和示例，使公开描述与可执行状态一致。
7. 实现 Terminal Native 视觉方向的英文默认、中文可切换、零构建依赖落地页。

## 3. 非目标

- 第一阶段不实现 JIT、AOT 或稳定的磁盘字节码格式。
- 不删除 AST Eval，也不要求 AST Eval 与 VM 具有相同内部实现。
- 不把 Pacman、线性回归或学习搜索策略硬编码进 VM 指令。
- Pacman 场景不安装、升级或删除软件包。
- 符号学习器不修改 Zio 运行时、不覆盖用户源文件、不获取 I/O 或进程能力。
- 科学计算 MVP 不引入 ndarray、BLAS 或 GPU 依赖。
- 落地页不伪装在线 REPL；在 WASM 执行器存在前，只展示真实示例与可复制命令。

## 4. 总体架构

```text
Source text
    │
    ▼
Reader ───────────────→ Sexp + Span
    │
    ▼
Macro Expander ───────→ Expanded Sexp
    │                         │
    │                         ├──→ AST Eval（语义参考、调试、差分测试）
    │                         │
    │                         └──→ ZIR Compiler
    │                                  │
    │                                  ▼
    │                             ZIR Optimizer
    │                                  │
    │                                  ▼
    │                         Bytecode Module + Constants
    │                                  │
    │                                  ▼
    └─────────────────────────────── Bytecode VM
                                       │
                 ┌─────────────────────┼─────────────────────┐
                 ▼                     ▼                     ▼
            Capabilities          RuntimeObserver         Value/Error
```

两个执行引擎共享 Reader、宏展开结果、Value、错误分类、模块语义和宿主 Capability。ZIR 是编译语义与以后 JIT 的稳定输入；字节码是 VM 的执行格式，可以独立演化。

## 5. ZIR

### 5.1 模块模型

```rust
pub struct ZirModule {
    pub name: ModuleName,
    pub constants: Vec<Value>,
    pub functions: Vec<ZirFunction>,
    pub entry: FunctionId,
    pub source: Option<SourceId>,
}

pub struct ZirFunction {
    pub name: Option<String>,
    pub required_arity: u16,
    pub has_rest: bool,
    pub local_count: u16,
    pub captures: Vec<Capture>,
    pub blocks: Vec<BasicBlock>,
}

pub struct BasicBlock {
    pub id: BlockId,
    pub instructions: Vec<ZirInstr>,
    pub terminator: ZirTerminator,
}
```

ZIR 显式表达局部槽位、闭包捕获、函数调用、尾调用、条件分支、集合构造、模块全局、异常处理和返回。每条指令保留可选 Span；ZIR 不包含宏节点，因为宏必须在进入编译器前完全展开。

### 5.2 名称解析

- 词法局部变量编译为 `SlotId`。
- 捕获变量编译为 upvalue 索引。
- 模块级绑定编译为模块符号索引，并保留运行时重定义语义。
- 无法静态解析的动态绑定使用明确的 `LoadDynamic`，不能静默退化为字符串查找。
- 特殊形式在 ZIR lowering 中处理，普通函数和 Generic Function 统一产生调用指令与调用点元数据。

### 5.3 第一批优化

第一阶段只实现可证明安全、容易差分验证的优化：

- 常量折叠：纯数值和不可变字面量。
- 常量池去重。
- 已知条件的死分支移除。
- 连续 `do` 表达式中无用中间值的 `Pop` 消除。
- 局部变量槽位复用。
- 普通尾位置编译为 `TailCall`。

不执行跨副作用调用重排，不假定用户函数纯净，不对 Generic Function 做不可失效的静态内联。

## 6. 字节码与 VM

### 6.1 字节码形态

第一版使用栈式字节码。指令按职责分组：

- 常量与栈：`Const`、`Nil`、`True`、`False`、`Pop`、`Dup`。
- 变量：`LoadLocal`、`StoreLocal`、`LoadUpvalue`、`StoreUpvalue`、`LoadGlobal`、`DefineGlobal`。
- 控制流：`Jump`、`JumpIfFalse`、`Loop`、`Return`。
- 调用：`Call`、`TailCall`、`CallNative`、`CallGeneric`。
- 闭包：`MakeClosure`、`CloseUpvalue`。
- 数据：List、Vector、Map 和 Tensor 构造指令。
- 错误：`PushHandler`、`PopHandler`、`Raise`。

操作数使用固定宽度编码开始，优先保证验证和调试简单。只有基准证明解码密度是显著热点后，才引入变长编码或 superinstruction。

### 6.2 执行状态

持久状态属于 Runtime：

```text
Runtime
├── global bindings
├── module registry
├── class registry
├── compiler cache
├── capabilities
└── runtime observer
```

每次执行拥有独立的 `VmInvocation`：

```text
VmInvocation
├── operand stack
├── call frames
├── open upvalues
├── handler stack
├── fuel remaining
└── trace context
```

VM 第一阶段单线程。可以在不同线程运行彼此隔离的 Runtime，但不允许多个线程同时共享可变 Runtime 状态。

### 6.3 动态代码与编译缓存

宏、DSL 和符号学习器会产生新的 S-expression。动态代码通过同一个 Compiler Service 编译为匿名模块，不能绕过验证器直接制造字节码。

编译缓存键包含：

- 忽略 Span 后的 Expanded Sexp 结构哈希；
- 编译器版本；
- 语义选项；
- 可见全局协议版本。

缓存只保存通过字节码验证的模块。模块重定义、Generic Function 方法变化或影响编译语义的配置变化必须使相关缓存失效。

### 6.4 引擎选择与切换

CLI 提供：

```text
zio --engine ast program.zio
zio --engine vm program.zio
```

在 VM 达到切换门槛前，默认仍为 AST。切换门槛为：

1. 语义差分 corpus 全部通过。
2. 官方示例全部通过两个引擎。
3. 错误类型和关键 Span 一致。
4. 核心 benchmark 无显著整体回退，并在函数调用、循环或局部变量访问中至少体现可重复收益。

切换后默认使用 VM，但 `--engine ast` 长期保留。

## 7. Capability 协议

### 7.1 原则

VM 不直接读取文件、启动进程或访问操作系统。宿主通过 Runtime Builder 显式提供能力；未提供能力时，调用返回类型化的 `CapabilityDenied`。

### 7.2 协议集合

```rust
pub trait ModuleHost {
    fn load_module(&self, name: &ModuleName) -> Result<ModuleSource, HostError>;
}

pub trait IoHost {
    fn read_text(&self, path: &Path) -> Result<String, HostError>;
    fn write_text(&self, path: &Path, content: &str) -> Result<(), HostError>;
}

pub trait ProcessHost {
    fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, HostError>;
}

pub trait NumericKernel {
    fn unary(&self, op: NumericUnaryOp, input: &Tensor) -> Result<Tensor, NumericError>;
    fn binary(&self, op: NumericBinaryOp, left: &Tensor, right: &Tensor)
        -> Result<Tensor, NumericError>;
    fn matmul(&self, left: &Tensor, right: &Tensor) -> Result<Tensor, NumericError>;
}
```

`ProcessRequest` 只接受可执行文件名和参数数组，不接受隐式 shell 字符串；包含超时、stdout/stderr 字节上限和环境变量白名单。StdProcessHost 是 CLI 提供的可选能力，测试使用 MockProcessHost。

## 8. 错误、安全与可观察性

### 8.1 错误模型

错误按阶段分类：

```text
ReaderError → ExpandError → CompileError → VmError → HostError
```

共同上下文包括错误类型、消息、Span、Zio 调用栈、底层 cause 和结构化 metadata。CLI 使用 SourceMap 输出文件名、行列、源码片段和调用栈；库使用者可以读取结构化错误，不必解析字符串。

### 8.2 执行限制

每个 VM Invocation 可配置：

- fuel 指令预算；
- 最大调用帧数；
- 最大 operand stack；
- 最大集合或 Tensor 元素数；
- Host call 超时和输出上限。

预算耗尽产生 `VmBudgetExceeded`，不使用 panic。符号学习候选在不含 I/O、Process 和 ModuleHost 的确定性子 Runtime 中运行。

### 8.3 Observer

RuntimeObserver 接收结构化事件：

- `CompileStarted`、`CompileFinished`；
- `CallEntered`、`CallReturned`；
- `HostCallStarted`、`HostCallFinished`；
- `VmBudgetExceeded`；
- `LearningGenerationScored`；
- `ErrorRaised`。

Trace 等级为 `off`、`errors`、`calls`、`instructions`。指令级跟踪默认关闭，benchmark 在 `off` 模式运行。

## 9. 性能工程

性能优化采用证据驱动流程：

1. 固化当前 AST 基线，记录版本、CPU、构建模式、命令和输入规模。
2. 分离 Reader、Expand、Compile 和 Execute 时间，避免把重复解析误算为 VM 执行成本。
3. 建立冷启动和热执行两类 benchmark。
4. 对 AST 与 VM 使用相同程序、参数和输出校验。
5. 每个优化提交都附带相关 criterion 对比；没有可重复数据的微优化不进入主线。

重点观察：

- 局部变量访问；
- 普通函数、闭包、NativeFn 和 GF 调用；
- 尾循环；
- List/Vector/Map 构造与遍历；
- Tensor 运算；
- 宏展开和动态编译缓存；
- 学习器候选的批量编译与执行。

## 10. 应用场景

### 10.1 Pacman 待更新检查

文件：`apps/pacman-updates.zio`。

行为：

1. 检查并优先执行 `checkupdates`。
2. 不可用时回退到 `pacman -Qu`。
3. 只读 stdout，解析为 `{:package name :current old :available new}` Map 序列。
4. 输出表格和待更新数量。
5. 无更新是成功结果，不是异常。

安全边界：不使用 shell，不执行 `pacman -Syu`，不请求 root，不修改系统。测试通过 MockProcessHost 覆盖命令不存在、无更新、正常更新、超时、非 UTF-8/异常输出和非预期退出码。

### 10.2 科学计算

新增不可变运行时值：

```rust
pub struct Tensor {
    pub shape: Arc<[usize]>,
    pub data: Arc<[f64]>,
}
```

MVP API：

- 创建与检查：`tensor`、`shape`、`tensor?`；
- 逐元素：加、减、乘、除和标量运算；
- 线性代数：`dot`、`transpose`、二维 `matmul`；
- 统计：`sum`、`mean`、`variance`；
- 应用：用梯度下降实现一元或多元线性回归示例。

广播第一阶段只支持标量与 Tensor、完全相同 shape；其他 shape 返回明确错误。所有 shape 乘积和索引计算检查溢出与边界。

### 10.3 数据流水线 DSL

DSL 使用 S-expression 声明：

```lisp
(pipeline updates
  (filter newer?)
  (map package-name)
  (aggregate count)
  (emit println))
```

宏展开为普通 Zio 的 `filter`、`map`、`reduce` 和最终 sink 调用。DSL 本身不拥有执行引擎、数据类型或副作用权限；Capability 由展开后调用的函数决定。

验收包括：

- `macroexpand` 输出稳定、可阅读；
- 临时符号不捕获用户变量；
- Pacman 结构化结果可直接进入 Pipeline；
- Tensor 样本可通过 Pipeline 做变换和聚合；
- 未知阶段、错误参数数量和非法顺序产生 ExpandError。

### 10.4 基于同像性的符号学习器

学习器输入训练样本和搜索配置，输出可运行的 Zio 函数：

```lisp
(learn-function
  '[[1 3] [2 5] [3 7]]
  {:ops '[+ - *]
   :constants '[-2 -1 0 1 2]
   :max-depth 4
   :beam-width 128
   :seed 7})
;; => (fn [x] (+ (* 2 x) 1))
```

搜索过程：

1. 从允许的操作、变量和常量构建受限 S-expression grammar。
2. 生成候选表达式并进行结构去重和常量折叠。
3. 通过 Compiler Service 编译；命中结构哈希时复用字节码。
4. 在无 I/O Capability、带 fuel 的子 VM 中对所有样本评分。
5. 使用 `loss + complexity_penalty` 排序，通过确定性 beam/evolution search 生成下一代。
6. 返回最小零损失程序；无零损失时返回最佳程序、损失、复杂度和停止原因。

相同样本、配置和 seed 必须产生相同输出。学习器通过 Observer 发布代数、候选数、缓存命中率、最佳损失和程序大小。

## 11. 文档真实性

新增 `docs/feature-matrix.md`，所有能力标记为：

- `stable`：官方示例和集成测试通过；
- `experimental`：API 可运行但可能变化；
- `planned`：只有规格或路线图，不宣称已实现。

同步更新：

- `README.md`：真实 workspace、测试状态、快速开始和应用入口；
- `docs/zio-architecture.md`：双引擎、ZIR、VM 与 Capability；
- `docs/eval-pipeline.md`：AST 与 VM 两条执行路径；
- `docs/roadmap.md`：按实际交付状态重排；
- `docs/adrs.md`：增加 VM、ZIR、能力边界和 Tensor 决策；
- `book/`：修正测试数量、组件状态和可运行示例；
- `examples/`：所有标为 runnable 的示例进入集成测试。

`tools/project-status` 从注册表和测试命令生成状态摘要；CI 比较生成结果与提交的快照，防止测试数、特殊形式、内置函数和组件状态再次漂移。

## 12. 落地页

### 12.1 技术边界

```text
site/index.html
site/styles.css
site/app.js
site/data/benchmarks.json
```

零构建依赖，可直接发布到 GitHub Pages 或任意静态服务器。英文默认，提供完整中文切换，语言偏好保存在 localStorage。

### 12.2 Terminal Native 视觉

- 黑色/墨绿色背景和荧光绿强调色；
- 等宽字体承载代码，sans-serif 承载解释；
- Hero 文案聚焦 “Programs that rewrite themselves”；
- 首屏展示学习器从样本产生 Zio 函数的真实输出；
- 内容依次介绍 VM、宏/DSL、Tensor、符号学习器、架构和基准；
- 提供 Get Started、Docs 和 Source 链接。

站点满足键盘导航、reduced-motion、移动端布局和 WCAG AA 对比度。性能数据必须来自版本化 JSON，并显示 CPU、日期、提交和复现命令。

## 13. 测试策略

### 13.1 差分测试

建立共享 corpus，分别通过 AST 和 VM 运行，比较：

- Value 的结构相等；
- 错误类型；
- 关键 Span；
- stdout/stderr 事件；
- Host 请求序列。

Corpus 覆盖字面量、词法作用域、闭包、递归、TCO、宏、模块、错误、ZOS、Tensor 和动态编译。

### 13.2 编译器与 VM 测试

- ZIR snapshot：验证 lowering 与控制流。
- 字节码 snapshot：验证常量池、跳转和闭包捕获。
- Bytecode verifier：拒绝非法跳转、栈下溢、错误常量索引和不闭合 handler。
- VM unit：逐指令和组合执行。
- Property tests：随机安全表达式的 AST/VM 等价性。

### 13.3 端到端测试

- 所有 `examples/*.zio` 中声明 runnable 的文件必须成功。
- 四个 `apps/` 场景具有黄金输出。
- Pacman 使用 MockProcessHost，不依赖测试机是否为 Arch Linux。
- 落地页用静态检查和浏览器检查验证语言切换、移动端、链接和可访问性。

## 14. 交付阶段

本文件是项目级总设计，不生成一个覆盖全部范围的巨型实施计划。六个 Phase 是可独立验收的子项目：每个 Phase 在开始前生成独立 implementation plan，在完成后执行测试、性能或内容门禁并接受复核；只有前一 Phase 提供的接口通过验收，后一 Phase 才能依赖它。首次实施只规划 Phase 0，随后依次规划 Phase 1 至 Phase 5。

### Phase 0：真实性基线

- 修复或重新标记现有失败示例。
- 固化 AST 行为、benchmark schema 和 feature matrix。
- 清理编译警告。
- 更新当前状态文档，但不提前宣称 VM 或应用已完成。

### Phase 1：ZIR 与编译器

- 实现 ZIR 类型、名称解析、lowering 和验证。
- 覆盖字面量、局部变量、分支、函数、闭包和尾调用。
- 建立 ZIR snapshot 与 AST 语义测试。

### Phase 2：字节码 VM 与语义对齐

- 实现字节码、编译、验证、VM、错误栈和 Observer。
- 接入宏展开、模块、NativeFn 和 ZOS 调用。
- 完成 AST/VM 差分 corpus 与性能基准。
- 达到切换门槛后把 VM 设为 CLI 默认。

### Phase 3：Capability 与三个应用

- 实现 ModuleHost、IoHost、ProcessHost、NumericKernel。
- 实现 Pacman、Tensor/线性回归和 Pipeline DSL。
- 加入安全、错误和端到端测试。

### Phase 4：符号学习器

- 实现 grammar、候选规范化、编译缓存、受限执行和确定性搜索。
- 输出学习程序与完整搜索指标。

### Phase 5：发布文档与落地页

- 完成所有文档同步和状态生成检查。
- 实现 Terminal Native 双语静态站点。
- 只发布通过自动验证的示例与 benchmark 数据。

## 15. 风险与缓解

| 风险 | 缓解措施 |
|---|---|
| VM 与 AST 语义漂移 | 同一 corpus 双引擎差分；AST 长期保留 |
| 编译器重构导致交付周期过长 | ZIR、VM、Capability、应用分阶段独立验收 |
| 字节码优化破坏动态 Lisp 语义 | 第一阶段仅做局部、可证明优化；全局重定义参与缓存失效 |
| 动态候选程序不终止 | fuel、帧数、集合大小限制；学习器无 Host Capability |
| 进程 API 形成命令注入 | executable/args 分离；不经过 shell；超时和输出上限 |
| Tensor 扩大 Value 复杂度 | 独立模块和 NumericKernel；MVP 限定 dense f64 与简单广播 |
| 性能数字不可复现 | 保存 benchmark 环境、提交、命令和原始 JSON |
| 文档再次漂移 | feature matrix、可执行示例和 project-status CI 检查 |

## 16. 完成定义

本设计整体完成需要满足：

1. AST 与 VM 共享 corpus 全部通过。
2. 四个应用均可通过 CLI 运行，并具有错误路径和黄金输出测试。
3. 性能报告区分编译与执行，公开数据可复现。
4. README、架构、ADR、路线图、教程和 feature matrix 与代码一致。
5. 双语 Terminal Native 落地页可静态发布，并且不包含未实现能力或虚构性能声明。
