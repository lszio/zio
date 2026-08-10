# 11 - ZOS：运行时对象系统

## 从 Lisp 到面向对象

ZOS = Zio Object System。它不像 Java/C++ 的类系统——ZOS 是**运行时对象模型**，允许你在运行时创建类、修改类、多分派函数。

```
ZOS 核心概念：
  Instance（实例）  ← 保存状态
  Class（类）       ← 定义结构
  Generic Function  ← 行为入口
  Method            ← 行为实现
                   ↓
           多分派！不是"对象.方法()"，
           而是"函数(对象1, 对象2)"根据参数类型选择方法
```

## 核心结构

### Class（类）

定义"点"类：

```lisp
(defclass Point ()
  ((x :initarg :x :accessor point-x)
   (y :initarg :y :accessor point-y)))
```

这段代码在 ZOS 中创建了一个 Class 对象：

```rust
// core/src/zos/object.rs
pub struct Class {
    pub name: String,                     // "Point"
    pub superclasses: Vec<ClassRef>,      // [TObject]
    pub slots: Vec<SlotDefinition>,        // [x, y]
    pub cpl: Vec<String>,                 // C3 类优先级列表
}

pub struct SlotDefinition {
    pub name: String,            // "x"
    pub initargs: Vec<String>,   // [":x"]
    pub initform: Option<Value>, // 默认值
    pub accessor: Option<String>, // "point-x"
}
```

### Instance（实例）

```lisp
(def p (make-instance 'Point :x 10 :y 20))
```

在运行时，实例是一个 `ZosInstance`：

```rust
// core/src/value.rs
pub struct ZosInstance {
    pub class: ClassRef,
    pub slots: HashMap<String, Value>,  // {"x": 10, "y": 20}
}
```

### Generic Function（通用函数）

```lisp
(defgeneric draw (shape))
```

GF 是"待分派函数"的入口：

```rust
// core/src/zos/gf.rs
pub struct GenericFunction {
    pub name: Symbol,
    pub lambda_list: Vec<ArgSpec>,     // [(shape, required)]
    pub methods: Vec<Method>,           // 已注册的方法
    pub dispatch_cache: DispatchCache,  // 分派缓存（加速）
}
```

### Method（方法）

```lisp
(defmethod draw ((p Point))
  (println "Drawing Point at" (point-x p) (point-y p)))
```

```rust
pub struct Method {
    pub specializers: Vec<Specializer>,  // [Exact("Point")]
    pub qualifier: MethodQualifier,      // Primary
    pub body: Arc<Function>,             // 方法体
}

pub enum Specializer {
    T,                     // 接受任何类型
    Exact(String),         // 只接受指定类
}

pub enum MethodQualifier {
    Primary,  // 主方法
    Before,   // :before — 在主方法前执行
    After,    // :after  — 在主方法后执行
    Around,   // :around — 包裹主方法
}
```

## 多分派

ZOS 的核心能力：**基于所有参数类型分派**，不只是第一个参数。

```lisp
(defgeneric collide (a b))

(defmethod collide ((a Asteroid) (b Asteroid))
  "两个小行星碰撞：爆炸！")

(defmethod collide ((a Asteroid) (b Spaceship))
  "小行星击中飞船：飞船受损！")

(defmethod collide ((a Spaceship) (b Asteroid))
  "飞船撞击小行星：改变航向！")
```

`collide` 方法根据两个参数的类型来选择：

- `(collide asteroid1 asteroid2)` → "爆炸！"
- `(collide asteroid ship)` → "飞船受损！"
- `(collide ship asteroid)` → "改变航向！"

这在传统的"对象.方法()"模型中很难实现——你需要双重分派。ZOS 天然支持。

## 分派流程

```rust
// eval.rs 中的 apply 函数
pub fn apply(func: Value, args: Vector<Value>, engine: &dyn EvalEngine)
    -> Result<TailResult, EvalError>
{
    match func {
        // ...
        Value::Object(obj) => {
            // 检查是不是 GenericFunction
            if let Some(gf) = obj.as_any().downcast_ref::<GFObject>() {
                return gf_dispatch(gf, args, engine);
            }
        }
    }
}
```

GF 分派的步骤：

```
apply: draw(point-instance)
       │
       ▼
1. 收集参数类型: [ClassRef("Point")]
       │
       ▼
2. 检查缓存: (type_id,) → 命中?
       │
       ├─ 命中 → 使用缓存的 Method 列表
       │
       └─ 未命中 → 扫描所有方法
           │
           ▼
3. 匹配 specializer: [Exact("Point")] → Point 的 method
           │
           ▼
4. 排序（按 CPL，更具体的优先）
           │
           ▼
5. Method Combination: :before → :primary → :after
           │
           ▼
6. 缓存结果
           │
           ▼
7. 执行: 绑定参数 → eval 方法体 → 返回结果
```

## Method Combination（方法组合）

一个 GF 可以有多个同名方法，通过 qualifier 组合：

```lisp
(defclass User () ((name :initarg :name)))

(defgeneric save (obj))

(defmethod :before save ((u User))
  (println "Saving user..."))

(defmethod :primary save ((u User))
  (println "  Writing to database..."))

(defmethod :after save ((u User))
  (println "Done saving user."))

(save (make-instance 'User :name "Alice"))
;; 输出:
;; Saving user...
;;   Writing to database...
;; Done saving user.
```

组合规则：
1. **`:before`**：从最具体到最不具体，全部执行
2. **`:primary`**：执行最具体的主方法
3. **`:after`**：从最不具体到最具体，全部执行

`:around` 包裹整个组合链：

```lisp
(defmethod :around save ((u User))
  (println "Transaction begin")
  (call-next-method)     ;; ← 调用内层组合（before → primary → after）
  (println "Transaction commit"))
```

## Class Registry

所有类注册在 `ClassRegistry` 中：

```rust
pub struct ClassRegistry {
    classes: Vec<ClassRef>,
}

impl ClassRegistry {
    pub fn register(&mut self, class: ClassRef) -> Result<(), EvalError>;
    pub fn find_by_name(&self, name: &str) -> Option<ClassRef>;
    pub fn class_of(&self, val: &Value, top: &ClassRef) -> ClassRef;
}
```

内置类层次结构通过 `make_builtin_classes()` 创建：

```
TObject
├── TInteger
├── TFloat
├── TString
├── TSymbol
├── TKeyword
├── TBoolean
├── TNil
├── TList
├── TVector
├── TMap
├── TFunction
├── TMacro
└── TObject（用户自定义类的根类）
```

## ZOS 特殊形式

ZOS 相关的特殊形式在 `special/zos_forms.rs` 中：

| 形式 | 用途 |
|------|------|
| `defclass` | 定义类 |
| `defgeneric` | 定义通用函数 |
| `defmethod` | 添加方法 |
| `call-next-method` | 在 around 方法中调用下一个方法 |
| `defpackage` | 声明包 |
| `try` | 条件处理（错误捕获） |
| `error` | 发出错误信号 |

## 动手实验

```lisp
;; 1. 定义形状系统
(defclass Shape () ())
(defclass Circle (Shape) ((radius :initarg :radius)))
(defclass Rectangle (Shape) ((w :initarg :w) (h :initarg :h)))

;; 2. 定义通用函数
(defgeneric area (shape))

;; 3. 添加方法
(defmethod area ((c Circle))
  (* 3.14159 (* radius radius)))

(defmethod area ((r Rectangle))
  (* w h))

;; 4. 使用
(area (make-instance 'Circle :radius 5))         ;; → 78.53975
(area (make-instance 'Rectangle :w 3 :h 4))      ;; → 12

;; 5. 反射 API
(class-of (make-instance 'Circle :radius 1))     ;; → Circle
(slot-value (make-instance 'Circle :radius 10) 'radius)  ;; → 10
```

## 对应源码

| 文件 | 内容 |
|------|------|
| `zos/mod.rs` | 模块入口 |
| `zos/object.rs` | ObjectHeader、ZosObject trait、Class 前向定义 |
| `zos/class.rs` | ClassRegistry、C3 线性化、内置类 |
| `zos/gf.rs` | GenericFunction、Method、MethodQualifier、分派 |
| `zos/package.rs` | Package 符号管理 |
| `special/zos_forms.rs` | defclass/defgeneric/defmethod/try 等特殊形式 |

## 核心记忆

> **ZOS = 运行时创建类 + 按参数类型多分派 + before/primary/after/around 方法组合。** 对象只保存状态，行为属于 Generic Function。
