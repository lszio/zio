# 11: ZOS — 统一运行时对象模型

> 从 `defclass` 到 MOP——Zio 的 AMOP 实现。

---

## 问题

Common Lisp 的 CLOS 有一个著名特性：**它不仅是对象系统，它本身是一个可以被扩展的对象系统**。这就是 AMOP（A Metaobject Protocol）——类、通用函数、方法、槽位，所有这些概念本身也是对象，可以通过 MOP 自省和扩展。

但 CLOS 是 ANSI 标准的一部分——2,000+ 页的规范。Zio 不需要完全兼容 ANSI CLOS。Zio 需要的是 **CLOS 的核心思想**：基于类的对象、多分派、方法组合、元对象协议。而且它需要在一个 Rust 托管的运行时中实现。

这就是 ZOS（Zio Object System）。

## 设计决策

### Object 不是所有东西

在 CLOS 中，一切都是 `standard-object` 的实例。Integer 也是。但在 ZOS 中不同：

```
ZOS 运行时对象模型
├── Immediate Object（内联，不分配）
│   Integer, Float, Boolean, Nil, Keyword
│
└── Heap Object（堆分配，带 ObjectHeader）
    Instance, Class, GenericFunction, Method,
    Package, Condition, Symbol
```

为什么 Integer 不是 ZOS Object？因为在 Rust 中，`Value::Integer(42)` 是一个 8 字节的 enum 变体，不需要堆分配、不需要 GC、不需要 `class-of` 查找。如果我们把所有值都装箱，一个整数加法就需要：

1. 解引用 GcHandle → 读取 ObjectHeader → 读到 class 是 `BUILTIN_CLASS_INTEGER`
2. 从 Object 的 data 区域读取实际整数值
3. 加法
4. 分配新的 Integer Object 存储结果

而 Immediate 版本只需要：

1. 从 Value 枚举读取整数
2. 加法
3. 构造新的 Value::Integer

**10 行 Rust 代码 vs 50 行。30ns vs 300ns。**

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

这引入了一个 vtable 调用开销（~5ns），但考虑到 GF 分派本身的成本（~500ns-1µs），这是可以接受的。

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

**下一篇：[12: 条件系统](12-condition-system.md)** — 从 try/catch 到 condition/restart。
