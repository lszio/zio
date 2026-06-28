# ZOS（Zio Object System）规范

> Version 0.1 — 统一运行时对象模型规范

---

## 1 概述

### 1.1 什么是 ZOS

ZOS（Zio Object System）是 Zio 的统一运行时对象模型（Unified Runtime Object Model）。它定义了对象如何存在、类型如何组织、行为如何分派、运行时如何扩展——在一个统一协议之下，不绑定任何特定领域框架。

**一句话定义**：ZOS 是 Zio 的 AMOP（A Metaobject Protocol）——一个运行时的对象结构、类型组织、行为分派、元编程协议。

### 1.2 设计目标

| 原则 | 含义 |
|------|------|
| **最小** | 核心只提供运行时最基本的能力。领域能力不进入 Core |
| **正交** | 各模块互相独立。Class 不依赖 Entity，GF 不依赖 Database，MOP 不依赖 Graph |
| **可扩展** | 四个官方扩展点：Reader Macro → Macro → MOP → Library |
| **运行时优先** | ZOS 描述的是运行时对象，不是语言语法 |
| **机制而非策略** | ZOS 只提供机制，不提供策略 |

### 1.3 职责边界

**ZOS 负责**：

| 组件 | 文件 |
|------|------|
| 运行时对象模型（Object / Type / Class / Slot） | `zos/object.rs`, `zos/class.rs` |
| 行为系统（Generic Function / Method / 多分派） | `zos/gf.rs`, `zos/method.rs` |
| Meta Object Protocol（MOP） | `zos/mop.rs` |
| 运行时反射 | `zos/reflection.rs` |
| 符号管理（Package） | `zos/package.rs` |
| 错误恢复（Condition System） | `zos/condition.rs` |

**ZOS 不负责**（全部由扩展库实现）：

| 能力 | 推荐库名 |
|------|----------|
| 数据持久化 | `zio-persistent` |
| Entity 模型 | `zio-entity` |
| Protocol 系统 | `zio-protocol` |
| Datalog 查询 | `zio-datalog` |
| Agent / AI Runtime | `zio-agent` |
| Actor 并发模型 | `zio-actor` |
| 图分析 | `zio-graph` |
| Clojure 风格集合 | `zio-persistent` |

---

## 2 运行时对象模型

### 2.1 Object 分类

ZOS 中一切运行时元素都是 Object。

```
Object
├── Immediate Object  (内联值，不分配堆内存)
│      Integer
│      Float
│      Character
│      Boolean
│      Nil
│      Keyword
│
└── Heap Object       (堆分配，带 ObjectHeader)
       Instance       — 类的实例
       Class          — 类本身
       MetaClass      — 类的类
       Function       — 用户定义的函数/闭包
       GenericFunction— 多分派函数
       Method         — GF 的方法实现
       Package        — 符号命名空间
       Condition      — 条件/错误
       Symbol         — 带命名空间的符号

Future Heap Object:
       Protocol
       Entity
       Actor
       Resource
```

### 2.2 Object 结构

每个 Object 都具有：

```rust
pub struct ObjectHeader {
    pub class: ClassRef,         // 指向所属类（或元类）
    pub flags: ObjectFlags,      // 标志位: mutable? persistent? etc.
    pub identity: Option<u64>,   // 可选唯一标识
}
```

**Object 不要求**：
- 一定拥有 Slot
- 一定可变
- 一定是 Class 的实例

**Object 是运行时最基本的单位**。不是所有 Object 都是 Class 的实例——内置类型（Integer、String 等）也是 Object，但它们由 VM 直接管理。

### 2.3 Immediate vs Heap

| 类型 | 存储 | 示例 | 扩展性 |
|------|------|------|--------|
| Immediate | 内联在 Value 枚举中 | `Value::Integer(42)` | 不可扩展 |
| Heap (Object) | 堆分配，Box<dyn ZosObject> | `Value::Object(Box<Instance>)` | 通过 MOP 可扩展 |

Immediate 类型在 Rust 层面由 `Value` 枚举内的直接变体表示。Heap Object 通过 `Value::Object(Box<dyn ZosObject>)` 统一接入，使 ZOS 可以动态扩展新的 Object 类型。

**过渡策略**：Phase 1 中，现有 `Value` 枚举保持，但新增 `Value::Object(Box<dyn ZosObject>)` 变体。所有 ZOS Heap Object 通过这个变体接入。Immediate 类型保持不变（不装箱，性能无损）。

---

## 3 类型系统

ZOS 采用**开放类型系统**（Open Type System）。第一版仅实现 Class。未来可以通过 MOP 扩展：

```
Type (抽象)
├── Class          (Phase 1)
│   └── MetaClass  (Phase 2)
├── Protocol       (Phase 3, 库)
├── Trait          (Phase 3, 库)
└── Interface      (Phase 3, 库)
```

这些扩展无需修改 ZOS Runtime。MOP 是所有扩展的入口。

---

## 4 Class

### 4.1 Class 结构

```rust
pub struct Class {
    pub name: String,
    pub superclass: Option<ClassRef>,   // 单继承
    pub slots: Vec<SlotDefinition>,      // 槽位定义
    pub metaclass: MetaClassRef,         // 元类（Phase 2 前固定为 standard-class）
    pub default_init: Option<Vec<Value>>, // 默认初始化参数
}
```

### 4.2 Class 能力

- 单继承（Phase 1），多继承（Phase 2+）
- 动态创建（`(defclass name super slots)`）
- 动态修改（`(defclass name super slots)` 重定义）
- `class-of` 反射
- `make-instance` 构造

### 4.3 类优先级（CPL）

多继承时，ZOS 使用 **C3 线性化算法**（同 Python 的 MRO）：

```
给定:  class C(A, B) 其中 A 继承 (A1, A2)
CPL(C) = [C, A, A1, A2, B, ... , object]
```

C3 保证：
- 子类优先于父类
- 单调性（local precedence ordering 被保留）
- 所有父类出现且只出现一次

---

## 5 Slot

### 5.1 Slot 定义

```rust
pub struct SlotDefinition {
    pub name: String,
    pub type_spec: Option<TypeSpec>,       // 类型约束（Phase 2+）
    pub default: Option<Value>,             // 默认值
    pub allocation: SlotAllocation,         // 分配策略
    pub reader: Option<String>,             // 自动生成读方法名
    pub writer: Option<String>,             // 自动生成写方法名
}

pub enum SlotAllocation {
    Instance,    // 每个实例独有（默认）
    Class,       // 类级别共享
}
```

### 5.2 Slot 存储策略

Slot 定义不决定对象存储方式。存储策略由 Runtime 决定。因此可以在不修改 Class 的前提下实现：

```
- Mutable Slot     — 用 RefCell<Value>
- Immutable Slot   — 用 Value（不可变）
- Persistent Slot  — 用 im::HashMap 快照
- Lazy Slot        — 用 OnceCell<Value>
```

Phase 1 统一使用 `RefCell<Value>`（可变实例槽位）。

### 5.3 Slot 访问

```lisp
;; 自动生成 reader/writer（通过 :accessor）
(point-x instance)       ;; slot reader
(setf (point-x instance) 42)  ;; slot writer

;; 通用访问
(slot-value instance 'x)      ;; 反射式访问
(slot-boundp instance 'x)     ;; 判断是否 bound
```

---

## 6 行为系统

### 6.1 核心设计

ZOS 将行为与对象解耦：**对象只保存状态，行为属于 Generic Function**。

```
对象        ←  保存状态 (Instance → Slot)
Generic Function  ←  行为入口 (name + dispatch)
    Method        ←  行为实现 (specializer + qualifier + body)
```

对象不拥有方法。方法不属于类。方法属于 Generic Function 并通过参数类型分派。

### 6.2 Function

Function 是第一类对象。包括：

| 类型 | 说明 |
|------|------|
| `CompiledFunction` | Zio 源定义或编译后的函数 |
| `Closure` | 带词法环境的函数 |
| `BuiltinFunction` | Rust 实现的 NativeFn |

支持：
- `lambda` 匿名函数
- `funcall` 函数调用
- `apply` 应用参数列表

Function 可以作为 Generic Function 的 `:implementation` 使用。

---

## 7 Generic Function

### 7.1 GF 结构

```rust
pub struct GenericFunction {
    pub name: Symbol,
    pub lambda_list: Vec<ArgSpec>,
    pub methods: Vec<Method>,
    pub dispatch_cache: DispatchCache,
}

pub struct DispatchCache {
    // 4-参数哈希键：((type_id1, type_id2, type_id3, type_id4) -> MethodList)
    cache: HashMap<(u64, u64, u64, u64), MethodList>,
}
```

### 7.2 多分派算法

GF 分派基于**全部必需参数的类型**。

```lisp
;; 单分派
(draw rect)            ;; 基于 rect 类型
(draw circle)

;; 多分派
(render mesh camera)   ;; 基于 mesh 和 camera 类型
(collide car wall)     ;; 基于 car 和 wall 类型
```

**分派步骤**：
1. 收集所有必需参数的类型 → `[TypeOf(arg1), TypeOf(arg2), ...]`
2. 检查缓存命中的 MethodList
3. 未命中：扫描方法列表，选出 `specializer ⊆ types` 的候选
4. 按 CPL 顺序排序（更具体的优先）
5. 执行 Method Combination
6. 缓存结果

### 7.3 4-参数缓存

```rust
impl GenericFunction {
    pub fn find_methods(&self, arg_types: &[ClassRef]) -> Option<&MethodList> {
        let key = (
            arg_types.first().map(|c| c.id).unwrap_or(0),
            arg_types.get(1).map(|c| c.id).unwrap_or(0),
            arg_types.get(2).map(|c| c.id).unwrap_or(0),
            arg_types.get(3).map(|c| c.id).unwrap_or(0),
        );
        self.dispatch_cache.cache.get(&key)
    }
}
```

- 键：前 4 个参数的类型 ID（对于 ≤4 参数的分派，常见情况）
- 未命中时，完整扫描并更新缓存
- 缓存失效：当 `defmethod`、`defclass`（重定义）时清除

### 7.4 通用函数定义

```lisp
;; 声明 GF
(defgeneric draw (shape))

;; 定义方法
(defmethod draw ((rect rectangle))
  (println "Drawing a rectangle"))

(defmethod draw ((circle circle))
  (println "Drawing a circle"))

;; 调用
(draw (make-instance 'rectangle))   ;; → "Drawing a rectangle"
```

---

## 8 Method

### 8.1 Method 结构

```rust
pub struct Method {
    pub specializers: Vec<TypeSpec>,      // 参数类型约束
    pub qualifier: MethodQualifier,       // primary / :before / :after / :around
    pub implementation: Box<dyn Fn(Vec<Value>, &dyn ZosRuntime) -> Result<Value, EvalError>>,
}
```

### 8.2 Method Combination

| 限定符 | 执行顺序 |
|--------|----------|
| `:around` | 最外层最先执行（最具体的优先） |
| `:before` | 执行顺序从最具体到最不具体 |
| `:after`  | 执行顺序从最不具体到最具体 |
| `primary` | 从最不具体到最具体（`:before` 之后，`:after` 之前） |

**执行流程**：

```
1. 收集 :around methods（最具体 → 最不具体）
2. 收集 :before methods（最具体 → 最不具体）
3. 收集 primary methods（最不具体 → 最具体）
4. 收集 :after methods（最不具体 → 最具体）

调用序列:
  around_1 → around_2 → ... → call-next-method
                              → before_1 → before_2 → ...
                              → primary_1 → primary_2 → ...
                              → after_n → ... → after_1
```

### 8.3 call-next-method

`call-next-method` 在 method 体内调用下一个方法（按方法组合顺序）。

```lisp
(defmethod draw :around ((shape shape))
  (println "Entering draw")
  (let [result (call-next-method)]   ;; 调用下一个 around 或 primary
    (println "Exiting draw")
    result))
```

---

## 9 Package

### 9.1 结构

```rust
pub struct Package {
    pub name: String,
    pub symbols: HashMap<String, Symbol>,
    pub exports: HashSet<String>,
    pub imports: Vec<(PackageRef, Vec<String>)>,
    pub aliases: HashMap<String, String>,
}
```

### 9.2 职责

- 符号命名空间管理
- `export` / `import` / `alias`
- 符号解析：查找当前包 → 查找导入包 → 报错

**Package 不承担模块管理职责**。模块加载、编译、代码组织由 `ModuleRegistry` 处理（属于 EvalEngine/Runtime 层，不属于 ZOS）。

### 9.3 语法

```lisp
(defpackage :zio.math
  (:use :zio.core)
  (:export :sin :cos :tan :sqrt)
  (:import :zio.constants :pi))

(in-package :zio.math)

;; 引用其他包的符号
zio.datalog/q
```

---

## 10 Condition System

### 10.1 定位

ZOS 保持 Common Lisp Condition System 的设计精神，但采用**分阶段实现**：

| Phase | 特性 | 控制流模型 |
|-------|------|-----------|
| Phase 1 | `(try body (catch Type handler))` | Result 传播 + 局部 restart |
| Phase 2+ | `handler-bind` + `signal` + 跨栈 `invoke-restart` | 扩展控制流 |

Phase 1 旨在提供**有用的错误恢复**，同时避免 Rust 的线性控制流与 CL 的非局部退出之间的冲突。

### 10.2 核心类型

```rust
pub struct Condition {
    pub type_spec: Symbol,            // 条件类型（:error, :warning, :custom）
    pub message: String,
    pub data: HashMap<Value, Value>,  // 额外数据
    pub restarts: Vec<Restart>,       // 可用的重启选项
}

pub struct Restart {
    pub name: Symbol,
    pub handler: Box<dyn Fn(&[Value]) -> Result<Value, EvalError>>,
}
```

### 10.3 Restart 传播

当 `invoke-restart` 被调用时，如果没有找到匹配的 restart，控制权沿调用栈向上传播。在 Phase 1 中，这通过 `Result` 的变体实现：

```rust
pub enum TailResult {
    Value(Value),
    Recur(Arc<Env>),
    Restart(Symbol, Vec<Value>),        // Phase 1: 函数内或通过栈传播
}
```

Phase 2 中（当有更多用户代码验证完整 Condition System 的需求时），引入 `handler_stack` 实现真正的跨栈恢复。

---

## 11 Meta Object Protocol

### 11.1 核心概念

MOP 是 ZOS 的核心扩展机制。所有运行时元数据通过 MOP 可编程扩展。

**分阶段实现**：

```
Phase 1: 无自定义 MOP，所有类使用 standard-class
Phase 2: MetaClass 注册表 + 自定义 class-of（有限 MOP）
Phase 3: 完整 MOP：
          - 自定义 Class（元类编程）
          - 自定义对象创建
          - 自定义 Dispatch
          - 自定义 Slot 分配
          - 完整的运行时反射
```

### 11.2 MOP 反射 API

```lisp
;; 元信息查询
(class-of obj)              ;; → obj 的类
(type-of obj)               ;; → obj 的类型标示
(slot-definitions obj)      ;; → obj 的槽位列表
(slot-value obj 'x)         ;; → obj 的槽位 x 的值
(methods gf)                ;; → gf 上的所有方法

;; 类反射
(class-name cls)            ;; → 类名
(class-superclasses cls)    ;; → 直接父类列表
(class-slots cls)           ;; → 槽位定义列表
(class-direct-subclasses)   ;; → 子类列表

;; GF 反射
(generic-function-name gf)
(generic-function-methods gf)
(generic-function-lambda-list gf)

;; Method 反射
(method-specializers m)
(method-qualifier m)
```

### 11.3 MOP 扩展路径

用户通过 MOP 可以实现：

| 场景 | MOP 入口 | 说明 |
|------|---------|------|
| 自定义成类 | `:metaclass` 参数 | 控制实例分配、slot 存储 |
| 自定义分派 | `compute-discriminating-function` | 改变 GF 的分派逻辑 |
| 自定义 slot 访问 | `slot-value-using-class` | 改变 slot 的读写行为 |
| 自定义实例化 | `make-instance-for-class` | 控制对象创建过程 |
| 持久化对象 | `allocate-instance` + 自定义 slot 读写 | 将 slot 存储在数据库中 |
| 日志代理 | 自定义 `slot-value-using-class` | 在读写时记录日志 |

---

## 12 反射

### 12.1 统一反射接口

所有反射基于 MOP：

| 函数 | 返回 |
|------|------|
| `(class-of obj)` | obj 的 Class |
| `(type-of obj)` | obj 的类型符号 |
| `(slot-definitions obj)` | 槽位定义列表 |
| `(slot-value obj slot)` | 槽位值 |
| `(setf slot-value)` | 设置槽位值 |
| `(slot-boundp obj slot)` | 槽位是否已绑定 |
| `(make-instance class initargs)` | 创建实例 |
| `(methods gf)` | GF 的方法列表 |
| `(method-specializers m)` | 方法的类型约束 |
| `(subtypep c1 c2)` | 类继承关系测试 |

### 12.2 `class-of` 实现

```rust
pub fn class_of(val: &Value) -> ClassRef {
    match val {
        // Immediate types → built-in classes
        Value::Nil => BUILTIN_CLASS_NIL,
        Value::Boolean(_) => BUILTIN_CLASS_BOOLEAN,
        Value::Integer(_) => BUILTIN_CLASS_INTEGER,
        Value::Float(_) => BUILTIN_CLASS_FLOAT,
        Value::String(_) => BUILTIN_CLASS_STRING,
        Value::Symbol(_) => BUILTIN_CLASS_SYMBOL,
        Value::Keyword(_) => BUILTIN_CLASS_KEYWORD,
        Value::List(_) => BUILTIN_CLASS_LIST,
        Value::Vector(_) => BUILTIN_CLASS_VECTOR,
        Value::Map(_) => BUILTIN_CLASS_MAP,

        // Heap Objects → stored class
        Value::Object(obj) => obj.header().class.clone(),

        // Functions → function metaclass
        Value::Function(_) => BUILTIN_CLASS_FUNCTION,
        Value::NativeFunction(_) => BUILTIN_CLASS_NATIVE_FN,
        Value::Macro(_) => BUILTIN_CLASS_MACRO,
    }
}
```

---

## 13 扩展模型

### 13.1 四个官方扩展点

```
优先度 ↓  →  1. Reader Macro (语法)
              2. Macro (语义)
              3. MOP (运行时)
              4. Library (模块)
```

| 层级 | 能力 | 适用场景 | 实现语言 |
|------|------|----------|----------|
| Reader Macro | 自定义语法 | `#[]` `#{}` 等读取器语法 | Zio |
| Macro | 代码变换 | DSL、控制结构、数据操作 | Zio |
| MOP | 运行时行为 | 对象创建、分派、槽位存储 | Zio + Rust |
| Library | 模块打包 | 独立功能包 | Zio + Rust |

### 13.2 扩展库注册表

ZOS 不内置包管理器，但定义库注册标准：

```
zio-persistent/          — 持久化数据结构
  zio-persistent.zio     — 主入口
  src/
    persistent-vector.zio
    persistent-map.zio
    persistent-set.zio

zio-datalog/             — Datalog 查询引擎
  zio-datalog.zio
  src/
    query.zio
    index.zio
    transact.zio

zio-agent/               — Agent 框架
  zio-agent.zio
  src/
    agent.zio
    tool-use.zio
    memory.zio
```

---

## 14 与现有 Value 类型的关系

### 14.1 统一模型

```
Immediate                    Heap Object (Value::Object)
─────────                    ──────────────────────────
Value::Integer → ZOS Object  Value::Function → ZOS Function
Value::Float   → ZOS Object  Value::NativeFn → ZOS BuiltinFunction
Value::Boolean → ZOS Object  Value::Macro    → ZOS Macro
Value::Nil     → ZOS Object
Value::String  → ZOS Object  [NEW] Instance  → ZOS Instance
Value::Symbol  → ZOS Object  [NEW] GF        → ZOS GenericFunction
Value::Keyword → ZOS Object  [NEW] Class     → ZOS Class
Value::List    → ZOS Object  [NEW] Package   → ZOS Package
Value::Vector  → ZOS Object
Value::Map     → ZOS Object
```

### 14.2 迁移路径

Phase 1 保持现有 `Value` 枚举，新增 `Value::Object` 变体：

```rust
pub enum Value {
    // 现有 Immediate 类型（保持不变）
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(String),
    Keyword(String),
    List(Vector<Value>),
    Vector(Vector<Value>),
    Map(HashMap<Value, Value>),

    // 现有可调用类型（逐步迁移到 ZOS Object）
    Function(Arc<Function>),
    NativeFunction(NativeFn),
    Macro(Macro),

    // ZOS 入口（Phase 1 新增）
    Object(Box<dyn ZosObject>),
}
```

其中 `ZosObject` trait 定义：

```rust
pub trait ZosObject: std::fmt::Debug + Send + Sync {
    fn header(&self) -> &ObjectHeader;
    fn as_any(&self) -> &dyn std::any::Any;
}
```

### 14.3 可调用对象

Value 的可调用性（在 `apply()` 中）扩展为：

```rust
pub fn apply(func: Value, args: Vector<Value>, engine: &dyn ZosRuntime) -> Result<TailResult, EvalError> {
    match func {
        Value::Function(f) => apply_function(f, args, engine),
        Value::NativeFunction(nf) => apply_native(nf, args, engine),
        Value::Macro(m) => apply_macro(m, args, engine),
        Value::Object(obj) => {
            if let Some(gf) = obj.as_any().downcast_ref::<GenericFunction>() {
                // GF 分派
                let arg_types: Vec<ClassRef> = args.iter()
                    .map(|a| engine.class_of(a)).collect();
                let methods = gf.find_methods(&arg_types, engine)?;
                apply_methods(methods, args, engine)
            } else if let Some(func) = obj.as_any().downcast_ref::<dynamic_function>() {
                // ZOS 层的函数对象
                func.call(args, engine)
            } else {
                Err(EvalError::not_a_function(...))
            }
        }
        _ => Err(EvalError::not_a_function(...)),
    }
}
```

---

## 15 实现路线

### Phase 1: 最小对象系统

**目标**：`defclass` + `defgeneric` + `defmethod` 可用，单继承、单分派优先。

| 组件 | 估算 LOC | 优先级 |
|------|----------|--------|
| ObjectHeader + trait | 80 | P0 |
| Built-in Class 注册表 | 120 | P0 |
| Class 定义（`defclass`） | 150 | P0 |
| Slot 定义 + 访问 | 120 | P0 |
| Generic Function 结构 | 100 | P0 |
| 4-参数 Dispatch Cache | 60 | P0 |
| Method 注册 + 单分派 | 120 | P0 |
| Method Combination 基础 | 80 | P1 |
| Package | 200 | P1 |
| Condition（简化） | 250 | P1 |
| `class-of` + 基础反射 | 100 | P1 |
| **总计** | **~1,380** | |

### Phase 2: MOP + 多分派

**目标**：完整多分派、Method Combination、MetaClass、运行时反射。

| 组件 | 估算 LOC |
|------|----------|
| C3 线性化 + 多继承 | 100 |
| Multi-dispatch（3+ 参数） | 120 |
| :before/:after/:around 完整组合 | 100 |
| MetaClass 注册表 | 80 |
| 自定义 class-of | 60 |
| 完整反射 API | 150 |
| Dispatch Cache 失效策略 | 80 |
| **总计** | **~690** |

### Phase 3: 官方扩展库

**目标**：通过 Macro + MOP 构建标准扩展生态。

| 库 | 说明 |
|----|------|
| `zio-persistent` | 持久化集合（不可变 Vector/Map/Set） |
| `zio-entity` | Entity 模型（带身份的对象） |
| `zio-protocol` | Protocol 系统（类似 Clojure protocols） |

### Phase 4: 高级生态

**目标**：高级运行时框架。

| 库 | 说明 |
|----|------|
| `zio-datalog` | 内存 Datalog 数据库 |
| `zio-graph` | 图查询 API |
| `zio-agent` | Agent 编排框架 |

---

## 16 与现有架构的集成

### 16.1 EvalEngine 扩展

ZOS 要求 `EvalEngine` trait 扩展为支持 ZOS 操作：

```rust
pub trait ZosRuntime: EvalRuntime {
    fn class_of(&self, val: &Value) -> ClassRef;
    fn find_and_apply_gf(&self, gf: &GenericFunction, args: &[Value])
        -> Result<TailResult, EvalError>;
    fn signal_condition(&self, c: Condition)
        -> Result<Option<TailResult>, EvalError>;
    fn invoke_restart(&self, name: &Symbol, args: &[Value])
        -> Result<TailResult, EvalError>;
}
```

非 ZOS 的嵌入场景只需实现 `EvalRuntime`，不承担 ZOS 复杂度。

### 16.2 编译器交互

```
(defclass point () (x y))
    │
    ▼
Reader → (defclass point nil (x y))
    │
    ▼
Macro Expansion → (eval-when (:compile-toplevel)
                    (register-class 'point nil '(x y)))
    │
    ▼
Compiler → 注册 Class 到当前 EvalContext
    │
    ▼
ZOS Runtime → Class 对象可被 class-of 反射
```

---

## 17 规范变更流程

ZOS 规范通过 ADR（架构决策记录）演进：

| ADR | 标题 | 状态 |
|-----|------|------|
| ADR-004 | Span 嵌入 Sexp | 待定 (Phase 1) |
| ADR-005 | EvalEngine 拆分为子 trait | 待定 (Phase 1) |
| ADR-006 | Value::Object 作为 ZOS 入口 | 待定 (ZOS Phase 1) |
| ADR-007 | Condition System 分阶段实现 | 待定 (ZOS Phase 1) |
| ADR-008 | 4-参数 Dispatch Cache | 待定 (ZOS Phase 1) |
| ADR-009 | Generic Function 优先于单分派函数 | 待定 (ZOS Phase 1) |

每个 ADR 记录：背景、决策、理由、代价、备选方案。
