export interface Theorem { icon: string; n: number; title: string; body: string }
export interface ArchLayer { title: string; desc: string }
export interface Roadmap { icon: string; accent: string; title: string; body: string }

export const theorems: Theorem[] = [
  { icon: '⚡', n: 1, title: '状态显式化', body: '零 <code>thread_local</code> 全局变量。所有状态在显式 <code>EvalContext</code> 中，在 Rust/WASM/浏览器端隔离运行。' },
  { icon: '🦀', n: 2, title: 'Rust 合同，Lisp 组合', body: 'Rust 提供底层 Native 契约与 WASM 零开销性能边界；Lisp 提供元编程与灵活组合层。' },
  { icon: '🌐', n: 3, title: 'JS / Web 原体深度互操作', body: '内置 <code>js/eval</code>、<code>js/console-log</code>、<code>js/dom-set-text</code> 等原语，直接在浏览器 WASM 环境操作 DOM 与 JS 全局上下文。' },
  { icon: '🧬', n: 4, title: '宏扩充 Eval 语义', body: '内置 <code>syntax-rules</code> 模式匹配与自动 gensym 命名空间，彻底消除变量捕获。' },
  { icon: '📦', n: 5, title: '协议优于实现', body: '基于 CLOS / AMOP 哲学的 ZOS 对象系统，Generic Function 支持基于 C3 线性化算法的多分派 (Multi-Dispatch)。' },
  { icon: '🔄', n: 6, title: '统一并发与 CSP', body: '内置 Future / Promise 异步延迟解算与 Go/Clojure 风格 CSP Channel（<code>chan</code>, <code>send!</code>, <code>recv!</code>）。' },
];

export const archLayers: ArchLayer[] = [
  {
    title: '应用程序与 Web 宿主层 (Application & WASM Layer)',
    desc: 'Astro 落地页 (Web REPL, <code>site/src/pages</code>) • zio-cli (CLI &amp; REPL) • grove serve (自学习控制面) • [Planned: LSP Server / DAP Debugger]',
  },
  {
    title: '自学习产品层 (grove: Learning Library & Product)',
    desc: 'learning/ (grove 宿主库: 存储·制品·隔离 worker·预算) • workers/torch/ (PyTorch 参考训练后端) • app/ (grove-app: CLI 与 HTTP API)',
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
    desc: 'AST Evaluator (TCO Trampoline) • ZOS Class/GF/Method/C3 • syntax-rules 卫生宏 • Future/Channel • JS FFI (js/eval, js/dom-set-text)',
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
    body: '卫生宏引擎、ZOS 多分派 & MOP 反射、Future/Promise 异步解算、CSP 通道、JSON 原语与 WASM / JS FFI 原体交付。',
  },
  {
    icon: '🌱', accent: 'purple', title: 'grove 自学习产品 (Experimental)',
    body: '基于 Zio 的自学习库与产品：多源反馈、代码/权重联合学习、检查点恢复、群体与模块演化。W00–W17 已实现并以契约测试验证。',
  },
  {
    icon: '🔮', accent: 'purple', title: 'Phase 5 & Phase 6 (扩展生态)',
    body: '扩展库（persistent / entity / protocol）、zio-datalog 图查询、zio-agent 编排框架 MVP 交付，Cranelift JIT 可行化探索。',
  },
];
