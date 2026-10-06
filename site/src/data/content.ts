export interface Theorem { icon: string; n: number; title: string; body: string }
export interface ArchLayer { title: string; desc: string }
export interface Roadmap { icon: string; accent: string; title: string; body: string }

export const theorems: Theorem[] = [
  { icon: '⚡', n: 1, title: '状态显式化', body: '零 <code>thread_local</code> 全局变量。所有状态在显式 <code>EvalContext</code> 中，在 Rust/WASM/浏览器端隔离运行。' },
  { icon: '🦀', n: 2, title: 'Rust 边界，Zio 自举方向', body: '当前运行时由 Rust 实现。Planned：Zio 承担展开、分析与编译器自举；Rust 保留最小运行时、宿主和性能原语。编译器自举不等于编辑器或 APP 的 Zio 化。' },
  { icon: '🌐', n: 3, title: 'JS / Web 原体深度互操作', body: '内置 <code>js/eval</code>、<code>js/console-log</code>、<code>js/dom-set-text</code> 等原语，直接在浏览器 WASM 环境操作 DOM 与 JS 全局上下文。' },
  { icon: '🧬', n: 4, title: '宏扩充 Eval 语义', body: '内置 <code>syntax-rules</code> 模式匹配与 gensym，已有卫生子集测试；不宣称完整卫生性或所有变量捕获均被消除。' },
  { icon: '📦', n: 5, title: '协议优于实现', body: '基于 CLOS / AMOP 哲学的 ZOS 对象系统，Generic Function 支持基于 C3 线性化算法的多分派 (Multi-Dispatch)。' },
  { icon: '🔄', n: 6, title: 'Future 与 Channel 原语', body: 'Current：Future 与 <code>chan / send! / recv!</code> 提供同步求值与队列演示，不是异步并发调度器。Planned：完整异步／取消语义。' },
];

export const archLayers: ArchLayer[] = [
  {
    title: '独立 APP 与静态站点 (Application & Web)',
    desc: 'Current: Astro 静态文档与 WASM REPL；grove serve 提供实验 CLI/API，不是已完成产品 Web。Planned: Grove 消费 Numa、Rill、Loom 独立库，负责实验、反馈、检查点、逻辑演化、独立评估、人工批准与产品 Web。',
  },
  {
    title: '官方独立库 Numa / Rill / Loom (Planned)',
    desc: 'Numa：数值计算、类型化连续数组/矩阵/向量与后端接口，不治理学习。Rill：参数、子命令、帮助、命令组合、终端 IO 与退出状态，不是语言求值器。Loom：模型、工具、会话、预算、取消、provider 与 ACP，不治理 Grove 学习/评估/发布。三库独立版本、按需引入，不是语言内建层；core 无反向依赖。Current: core / cli / ai / learning / app 五 crate，zio-ai 未改名，zio-cli 二进制不是 Rill 库。',
  },
  {
    title: '扩展库生态层 (Extension Library Layer)',
    desc: 'lib/zio/protocol.zio (Protocol 系统) • lib/zio/entity.zio (Entity 模型) • lib/zio/persistent.zio (持久化集合) • lib/zio/learn/* (model, recipes, population)',
  },
  {
    title: '标准库层 (Standard Library Layer)',
    desc: 'core.zio (内嵌核心宏 defn, when, cond, -&gt;, -&gt;&gt;, assoc, dissoc, get-in, assoc-in)',
  },
  {
    title: '运行时 &amp; ZOS 对象层 (Runtime &amp; ZOS Layer)',
    desc: 'Current: AST Evaluator • ZOS Class/GF/Method/C3 子集 • 宏卫生子集 • 同步 Future/Channel • JS FFI。ZOS object/class/GF/method/MOP 方向保留在语言核心；Planned: 完整 MOP 与 core 职责收敛。',
  },
  {
    title: '前端语法层 (Frontend Reader)',
    desc: 'Tokenizer • Parser • Sexp AST (内嵌 Span 源码定位) • Reader Macro',
  },
  {
    title: 'Rust 宿主层 (Rust Host Layer)',
    desc: 'EvalContext • EvalRuntime • ModuleRegistry • IoHost • NativeFn • wasm-bindgen',
  },
];

export const roadmap: Roadmap[] = [
  {
    icon: '✅', accent: 'green', title: 'Phase 1 & Phase 2',
    body: '核心稳定化与 ZOS 基础。实现 Span 错误定位、全 TCO 尾递归、IoHost 抽象与 Class/GF/Method 子集。',
  },
  {
    icon: '🚀', accent: 'cyan', title: 'Phase 3 & Phase 4 & WASM',
    body: 'Current：syntax-rules 卫生子集、ZOS 多分派子集、同步 Future/Channel 演示、JSON 与 WASM / JS FFI。Planned：完整 MOP、完整卫生性与异步调度；历史阶段名称不代表这些目标已交付。',
  },
  {
    icon: '🌱', accent: 'purple', title: 'Grove 独立 APP (Planned)',
    body: '真实 LLM 生成代码，经解析、能力检查与隔离执行，以错误和证据修订；Web 展示源码/路径/diff，反馈产生逻辑候选，独立评估后人工批准正式版本。历史 CPU demo 不代表此闭环完成。',
  },
  {
    icon: '🔮', accent: 'purple', title: '核心自举与独立库 (Planned)',
    body: 'ZOS 与展开/分析/编译器工具链仍属 Zio 语言；Planned：Zio 自举，Rust 保留最小宿主。Numa、Rill、Loom 是 Grove 按需消费的官方独立库，不是内建语言能力或已可安装包。Loom 的 ACP 为 Agent Client Protocol，先 client 与 teacher adapter、后 server。Grove 自动实验仅限批准预算/编辑范围，可信基线与发布守卫不可自改。',
  },
];
