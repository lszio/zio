# 11: ZOS — 统一运行时对象模型

> 2026-10-07 目录更新：本文教学代码与旧阶段/验收描述保留历史语境；源码链接和运行路径已映射到当前位置。现行目标是 `libs/` 的 Zio 库与 `apps/grove/` 的 Zio 业务迁移，Rust Grove 业务尚未整体重写；通用 Rust transport/host adapter 在 `contribs/native/loom/`。当前归属见[架构](../docs/zio-architecture.md)与[批准目录计划](../docs/superpowers/plans/2026-10-07-language-first-layout.md)。

> 从 `defclass` 到 MOP——历史 ZOS 设计稿，不是完整 AMOP 已交付声明。

---

## 阅读范围

本文保留早期 Phase 1/2/3 的设计推演；下文布局、缓存、条件系统代码及阶段“交付”描述均是当时的方案/目标，不是当前实现验收。当前对象/Class/GF/Method 路径与 MOP 目标请以[语言架构](../docs/zio-architecture.md)、[批准基线](../docs/self-learning-architecture.md)和[实施计划](../docs/superpowers/plans/2026-10-05-zio-grove-convergence.md)为准；未实现能力为 Planned。ZOS 留在语言核心，不因 Grove 产品化或编译器自举而被移成可选业务库。

> **2026-10-05 命名同步**：Numa（计算）、Rill（CLI 组合）、Loom（Agent
> harness）是按需安装、独立版本发布的官方库，不是 Zio/ZOS 内置特性。
> Grove 是消费语言与三库的独立应用，持学习目标、实验、反馈、检查点、
> 独立评价、人工批准、逻辑演化与 Web；核心无反向依赖。三库新增能力仍 Planned。
> 当前 Rust `loom` adapter 位于 `contribs/native/loom/`，`zio-cli` 是未来消费 Rill 的可执行宿主；
> 下文历史包名、代码与阶段记录不改名。正式短名不指定注册包 ID，
> 未来目录/逻辑命名空间见[术语表](../docs/glossary.md)，边界见
> [ADR-019](../docs/adrs.md#adr-019-grove-独立应用与同像性逻辑演化)。


## 问题

Common Lisp 的 CLOS 有一个著名特性：**它不仅是对象系统，它本身是一个可以被扩展的对象系统**。这就是 AMOP（A Metaobject Protocol）——类、通用函数、方法、槽位，所有这些概念本身也是对象，可以通过 MOP 自省和扩展。

但 CLOS 是 ANSI 标准的一部分——2,000+ 页的规范。Zio 不需要完全兼容 ANSI CLOS。Zio 需要的是 **CLOS 的核心思想**：基于类的对象、多分派、方法组合、元对象协议。而且它需要在一个 Rust 托管的运行时中实现。

这就是 ZOS（Zio Object System）。

## 设计决策

### Object 不是所有东西

Common Lisp 的数字也是对象且有对应类，但不是 `standard-object` 实例；内联值与堆对象的实现布局不等于语言层面“是否有类”。这里讨论历史 ZOS 的表示选择：

```
ZOS 运行时对象模型
├── Immediate Object（内联，不分配）
│   Integer, Float, Boolean, Nil, Keyword
│
└── Heap Object（堆分配，带 ObjectHeader）
    Instance, Class, GenericFunction, Method,
    Package, Condition, Symbol
```

`Value::Integer(42)` 的整数载荷是 8 字节，不等于整个 enum 只有 8 字节。内联表示可避免每个整数独立装箱，但仍有求值、动态类型检查与调用开销；若把所有值装箱，概念流程会是：

1. 解引用 GcHandle → 读取 ObjectHeader → 读到 class 是 `BUILTIN_CLASS_INTEGER`
2. 从 Object 的 data 区域读取实际整数值
3. 加法
4. 分配新的 Integer Object 存储结果

而 Immediate 版本只需要：

1. 从 Value 枚举读取整数
2. 加法
3. 构造新的 Value::Integer

这是减少装箱和间接访问的设计动机，不是已经测得的耗时对比；性能须使用相同工作负载与环境的[基准记录](../benchmarks/README.md)验证。

所以 ZOS 的分类是务实的：**Immediate 类型保持性能，Heap Object 获得扩展性。**

### MOP 从 Phase 2 开始

完整的 MOP 允许用户自定义元类：

```lisp
(defclass my-class () (...)
  (:metaclass tracking-meta-class))
```

这需要 `standard-class` 本身是 `meta-class` 的实例，而 `meta-class` 又是 `standard-class` 的实例——一个自指的循环。在 Rust 中实现这个自指需要 `ClassRef = Arc<Class>` 和运行时注册表。

Phase 1 中，我们跳过自定义元类。所有 Class 的 metaclass 都是 `standard-class`。这意味着：

- `defclass` 只能使用标准元类
- `make-instance` 使用标准分配策略
- `slot-value` 使用默认的 `RefCell<Value>` 存储

但这已经足够构建**对象**、**继承**、**多分派**、**方法组合**。完整的 MOP 在 Phase 2，当用户真的需要自定义分派逻辑时再实现。

### Generic Function 是独立的对象

在大多数 OOP 语言中，方法属于类。在 ZOS 中，方法属于 Generic Function：

```
(defgeneric draw (shape))
    │
    ├── (defmethod draw ((rect rectangle)) ...)
    ├── (defmethod draw ((circle circle)) ...)
    └── (defmethod draw ((triangle triangle)) ...)

draw 是一个对象（GenericFunction 的实例）。
rectangle 的实例不拥有 draw 方法。
```

这有什么好处？

**你可以给已存在的类添加方法而不修改它：**

```lisp
;; 给 Zio 内置的 Number 类添加 serialize 方法
(defgeneric serialize (obj))

(defmethod serialize ((n number))
  (number-to-string n))

(defmethod serialize ((s string))
  (str "\"" s "\""))
```

这在 Rust 的 trait 系统中也类似——但 trait 是编译时的。ZOS 的 Generic Function 是**运行时的**：

```lisp
;; 在运行时添加方法
(eval '(defmethod serialize ((v vector))
         (str "[" (map serialize v) "]")))

;; 现在 vector 也可以 serialized
(serialize [1 2 3])   ;; → "[123]"
```

## Value::Object 的实现

ZOS 通过 `Value::Object(Box<dyn ZosObject>)` 接入现有 Value 系统：

```rust
pub trait ZosObject: Debug + Send + Sync {
    fn header(&self) -> &ObjectHeader;
    fn as_any(&self) -> &dyn Any;
}

pub struct ObjectHeader {
    pub class: ClassRef,       // Arc<Class>
    pub flags: ObjectFlags,    // 位标志：mutable, persistent, etc.
    pub identity: Option<u64>, // 可选唯一身份
}

pub struct Instance {
    pub header: ObjectHeader,
    pub slots: RefCell<HashMap<String, Value>>,
}
```

当 `apply()` 遇到 `Value::Object` 时，它尝试 `downcast_ref::<GenericFunction>()`。如果成功，执行 GF 分派。否则，检查是否是可调用的 Function 子类型。

这个方案增加 vtable 间接调用，是否值得应以实测 GF 分派成本与扩展需求判断；原设计没有提供可复现测量，因此不保留纳秒级估算为性能证据。

## 4-参数缓存

Generic Function 的最大性能风险是每次调用都扫描方法列表。ZOS 使用简单的哈希缓存：

```rust
struct DispatchCache {
    cache: RefCell<HashMap<(u64, u64, u64, u64), MethodList>>,
}

impl DispatchCache {
    fn lookup(&self, types: &[ClassRef]) -> Option<MethodList> {
        let key = (
            types.first()?.id(),
            types.get(1).map(|c| c.id()).unwrap_or(0),
            types.get(2).map(|c| c.id()).unwrap_or(0),
            types.get(3).map(|c| c.id()).unwrap_or(0),
        );
        self.cache.borrow().get(&key).cloned()
    }

    fn insert(&self, types: &[ClassRef], methods: MethodList) {
        let key = (...);
        self.cache.borrow_mut().insert(key, methods);
    }

    fn clear(&self) {
        self.cache.borrow_mut().clear();
    }
}
```

4 个 u64 在栈上，一个 `HashMap` 查找。缓存命中时 O(1)，未命中时一次 O(n·m) 扫描后缓存。

## 条件系统：Phase 1 版

完整的 Common Lisp Condition System 允许 `invoke-restart` 穿越多个栈帧。在 Rust 的线性控制流中，这需要 `std::panic::catch_unwind` 或显式的 continuation。Phase 1 不做这个。

Phase 1 的条件系统是 **基于 Result 传播的**：

```rust
pub enum TailResult {
    Value(Value),
    Recur(Arc<Env>),
    Restart(Symbol, Vec<Value>),  // 沿调用栈传播
}
```

`Restart` 变体沿调用栈向上传播，直到遇到匹配的 `with-restart` 或 `try/catch`。这类似于用 `Result` 的 `Err` 变体做控制流——但在 Zio 语义层面是有名重启（named restart）而不是匿名的 catch。

```lisp
(defun risky-file-read [path]
  (with-restart (use-default (fn [msg] "default content"))
    (if (file-exists? path)
      (slurp path)
      (error "file not found"))))

;; 调用方可以选择：
(try
  (risky-file-read "/nonexistent")
  (catch "file not found"
    (invoke-restart 'use-default "using default")))
```

这个设计满足 80% 的实用需求。完整的跨栈 Condition System 在 Phase 3 以后——如果用户社区证明需要的话。

## 总结

ZOS 不是 CLOS 的克隆。它是 Zio 的运行时对象协议——更小、更务实、在 Rust 的类型边界内最大化扩展性。

**Phase 1 交付：** 你可以用 `defclass` 定义类、用 `defgeneric` / `defmethod` 定义多分派函数、用 `class-of` 做反射、用 `slot-value` 访问槽位。所有 Class 使用标准元类，没有自定义 MOP。

**Phase 2 交付：** 完整的 MOP。自定义元类、自定义分派、自定义槽位存储。`defclass` 的 `:metaclass` 参数生效。

**Phase 3 交付：** 通过 MOP 构建的官方扩展库——`zio-persistent`、`zio-entity`、`zio-protocol`。

## 在代码中

- 打开 [`docs/zos-spec.md`](../docs/zos-spec.md) 看完整规范
- 打开 [`docs/adrs.md`](../docs/adrs.md) 看 ADR-006 到 ADR-009
- 打开 [`docs/glossary.md`](../docs/glossary.md) 看术语

```bash
# 在实现开始后：
cargo test --test zos  # ZOS 测试套件
```

---

**计划文章：12: 条件系统（Planned，尚无文章文件）** — 从 try/catch 到 condition/restart。
