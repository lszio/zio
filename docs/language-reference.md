# Zio 当前语言参考

日期：2026-10-11。范围：L00 语言合同、L01 显式元循环执行面与 L02 首轮工具链自举；不代表镜像、应用导出或完整发行已经交付。

本文是写当前 Zio 程序时的语义入口；历史教程与设计文档冲突时，先依据当前实现与行为合同核对，不能把历史目标当成已支持功能。语言由表达式、值、作用域和可观察行为定义，不由某个 Rust trait 或宿主内存布局定义。

## 状态与证据

- **现有公开语义**：下文描述当前 reader、AST 求值器、标准库及宿主边界；例子中的“预期”是语义说明，未注明实跑的例子不代表本轮已执行。
- **三执行面合同**：`langs/core/tests/toolchain_contract.rs` 使用 fresh context 对照 AST、显式编译／VM 与 Zio 元循环；编译期宏副作用仍按各入口的阶段合同分别验证，不能强行混成同一时点。
- **Zio 元循环执行面**：`langs/interpreter/` 中的 reader、evaluator、expand 用 Zio 实现；默认执行器不变，显式入口见下文。Rust 仍引导安装这些实现并提供值、环境、对象、宿主及编译产物执行机制。
- **规划**：镜像、独立应用导出、WASI、原生 JS／Rust 引擎及 inline／原始外语块见[语言工具链设计](superpowers/specs/2026-10-11-zio-language-toolchain-design.md)与[实施计划](superpowers/plans/2026-10-11-zio-language-toolchain.md)，不是当前语法。

实施及实跑状态以[特性矩阵](feature-matrix.md)和实施计划的证据记录为准。本文不预填新合同通过，不作性能推断。

### 显式 Zio 元循环入口

```zio
(zio-meta-read "(+ 20 22)" "sample.zio") ; 返回 SyntaxNode 向量，不执行目标源码
(zio-meta-eval "(+ 20 22)" "sample.zio") ; 返回 42；当前环境中的定义会保留
```

Rust 嵌入端对应 `interpreter::read_source`／`interpreter::eval_source`。目标源码由 Zio reader 读取；普通函数、宏展开及模块初始化由 Zio evaluator 执行，不把目标表达式交回 Rust AST。原生函数按真实绑定身份识别；用户自定义一个名为 `eval` 的 native binding 不会被替换。模块授权、导出、加载失败清理与宿主 I/O 权限沿用当前 context，不因切换执行面增加权限。

控制状态和 continuation 显式保存；普通调用、tail/recur 及内置 map/filter/reduce/apply/sort/future-call 回调不以目标递归嵌套 Rust evaluator。外部嵌入者提供的同步 native callbacks 仍可能重入宿主调用栈；环境句柄记录随创建数量增长，不宣称常量空间或性能提升。future/chan 保持同步语义，syntax-rules 保持现有卫生子集。本入口不是镜像、应用导出或 Rust-free 发行包。

### 编译前端与自举命令

```sh
cargo run -p zio-cli -- self-build --verify
# 可选：--out-dir /tmp/zio-stages；默认 target/zio-self-build/
bash tools/smoke-self-build.sh
```

前端由 `langs/interpreter/{reader,expand,evaluator}.zio` 与
`langs/compiler/{analyze,compiler}.zio` 组成：读取目标源码、宏展开、词法绑定、
尾位置与 capture/global 选择由 Zio 表达。Rust 保留引导解析、值/环境/错误投影、
指令执行、wire 校验和 float bits，不把目标源码交回 Rust reader 或 expander。

Stage 0 是解释执行的 Zio 前端；Stage 1 编译整个前端，Stage 2/3 各在 fresh
context 安装上一代完整制品后再编译。旧前端绑定全部移除，不能借用缺失 helper。
`--verify` 读取写出的三个 JSON 文件，逐字节比较，并在 fresh context 验证
四个编译入口与 sum=10、closure=15。构建 context 使用空模块根，不借用外部模块。
这证明首轮便携字节码固定点，不代表 Rust-free、机器码 AOT、镜像或全套发行。

### 镜像与编译制品

```sh
zio image save PROGRAM.zio -o IMAGE [--root NAME]...  # 评估并保存静止镜像
zio image inspect IMAGE                              # 只读元数据，不恢复
zio image run IMAGE [--eval EXPR] [--grant-dir DIR]... # 空模块根恢复后执行
zio compile PROGRAM.zio -o OUT                        # 编译为便携模块
```

`save` 的 roots 默认是程序结果（名 `root-0`），`--root NAME` 保存指定绑定。
`run` 在空模块根的 fresh context 恢复，按名绑定 roots，再求值 `--eval`；
文件访问仅限 `--grant-dir` 授予的根目录，根外读取按拒绝处理——镜像本身不携带
任何权限或凭据。`inspect` 只做结构与预算校验，不触发恢复。

```sh
zio build examples/toolchain/app.zio --target native -o /tmp/zio-app
/tmp/zio-app
```

`build` 产出单文件应用：native 为 runtime 二进制 + JSON payload + 带校验和的
footer；`--target browser` 产出单个 HTML（glue URL + 内联 base64 payload）；
`--target wasm-wasi` 产出单个 `.wasm`（WASI runtime + 编译进模块的 payload）。
三者共用同一 payload 合同，由原生启动钩子或 `ZioSession.run_bundle` 执行。
输入支持源码（`.zio`）、便携模块（`.zioc`）与镜像（`.zio-image`，需
`--entry EXPRESSION`）。模块依赖在构建期按真实 reader 扫描声明闭包，缺失
即拒绝构建；未声明的 `require` 在运行时也无法触碰 checkout。WASI 应用的
宿主文件访问需要 preopen **且** `--grant DIR`；浏览器专有依赖（`js/*`）
在 WASI 构建期拒绝。


## 表达式与值

程序是零个或多个表达式；顶层依次求值，结果为最后一个值，空程序返回 `nil`。`;` 开始行注释。非空列表 `(head arg...)` 是调用、特殊形式或宏调用；空调用列表 `()` 求值为 `nil`，但引用的空列表 `'()` 是列表值，不等同于 `nil`。

| 名义值类别 | 写法／获得方式 | 求值规则 |
| --- | --- | --- |
| 空值、布尔 | `nil`、`true`、`false` | 自求值 |
| 整数、浮点 | `42`、`-3`、`1.5`、`1e3` | 自求值；有符号 64 位整数与双精度浮点是不同类别 |
| 字符串、字符 | `"hello"`、`#\a`、`#\space`、`#\newline`、`#\tab` | 自求值；字符是单个 Unicode 字符 |
| 关键字 | `:ready` | 自求值的名称值，不查找变量 |
| 符号 | `x`；作为数据写 `'x` | 未引用时按环境查找 |
| 列表、向量、映射 | `'(1 2)`、`[1 2]`／`#(1 2)`、`{:x 1}` | 列表通常是调用；向量元素与映射键值会求值 |
| 集合 | `#{1 2 2}` | reader 展开为 `(set 1 2 2)`；标准库用成员到 `true` 的映射表示，重复项合并 |
| 函数、宿主函数、宏 | `fn`、预装原语、`defmacro` | 可传递的值；宏接收未求值的形式 |
| 对象、可变字节缓冲 | ZOS 构造、`bytes` 等原语 | 对象／缓冲操作由对应接口提供，不是源码字面量 |
| future／promise、channel | `future-call`、`promise`、`chan` | 当前同步能力，见宿主章节 |

**只有 `nil` 与 `false` 为假。** `0`、`""`、空向量、空映射、引用的空列表都为真。集合不是另一个独立运行时值类别；`(get #{:a :b} :a)` 返回 `true`，缺项可用 `(get s item nil)`。

字符串支持 `\n`、`\r`、`\t`、`\\`、`\"`；未知转义保留反斜线与后续字符，不宣称 JSON 的全部转义语法。未闭合字符串、缺失括号、奇数映射项及 quote 后缺少表达式是 reader 错误。映射迭代／打印的键顺序不属于语义合同，不要依赖它组织副作用。

### 数值合同

全整数的 `+`、`-`、`*` 保持整数；二元／多元整数 `/` 向零截断，例如 `(/ 7 2)` 预期为 `3`。混入浮点时算术转为浮点；单参数 `/` 是浮点倒数，而单参数 `-` 是取负。`+`、`*` 当前也要求至少一个参数，不能假定零参数单位元。除零报错。

整数加减乘及二元除法的溢出路径当前按 64 位回绕处理；不要将此推广为每个数值原语都具有统一的溢出政策。排序 `<`、`<=`、`>`、`>=` 是数值排序，整数比较保持精度，整数／浮点混合比较不先把大整数无条件转成浮点；非有限浮点排序及字符串排序拒绝。`sqrt` 接受一个非负有限标量数值并返回浮点，不是张量操作。

显示相同不等于类别相同：浮点 `1.0` 的打印可以是 `1`。行为对照必须比较值类别与值，不能只比较 `to_string`。

## 引用与模板

`'form` 是 `(quote form)` 的简写，返回形式对应的数据，不求值其中的符号或列表：

```lisp
'(+ 1 2)                    ;; 预期列表 (+ 1 2)，不是 3
(quote [x :ready])           ;; 向量内的 x 是符号数据
(let [x 3] `(+ ~x 1))        ;; 预期列表 (+ 3 1)
`[0 ~@(list 1 2)]            ;; 预期向量 [0 1 2]
```

反引号是 `quasiquote`，`~` 是 `unquote`，`~@` 是 `unquote-splicing`。在匹配的模板深度，`~` 求值；`~@` 将列表或向量的元素插入外围列表／向量。嵌套反引号增加深度，内层 unquote 保留为数据，直到对应深度被处理。映射模板可对键和值作 unquote，但不支持将 `~@` 当作映射键值拼接。模板外 unquote、模板根处拼接或拼接非序列值均拒绝。

## 求值次序与控制流

普通调用先求值 head，再从左到右求值实参，然后调用；错误终止后续求值。向量元素从左到右求值。映射每对键先于其值求值，但不承诺各对之间的顺序。宏的原始参数不在展开前求值；展开结果再作为表达式求值。

- `(do e1 ... en)` 顺序执行，返回最后一个值；空 `do` 返回 `nil`。
- `(if test then [else])` 只执行所选分支，无 else 且 test 为假时返回 `nil`。
- `(and ...)` 是短路特殊形式：返回首个假值，否则返回最后一个值；无参数返回 `true`。
- `(or ...)` 是短路特殊形式：返回首个真值，否则返回最后一个假值；无参数返回 `nil`。
- `cond` 接收**非空列表子句**，不是 Clojure 的平铺 test／expr 对，也不是向量子句。依次测试，执行首个命中子句的顺序 body，返回其最后值；无命中返回 `nil`。`else` 是子句首位置的特殊标记，应放最后。

```lisp
(cond ((< n 0) :negative)
      ((= n 0) :zero)
      (else :positive))
(and false (error "不执行"))
(or :ready (error "不执行"))
```

标准库同时定义名为 `and`／`or` 的**函数值**。直接 head 为 `and`／`or` 的语法走特殊形式；传给 `apply` 或赋给其他函数名时走普通函数，所有实参已经求值，不能短路。标准库 `and` 函数将假分支归一成 `false`，不同于特殊形式保留原假值 `nil`。

## 绑定、作用域与变异

符号查找从当前词法作用域向外；闭包保留定义处环境，不使用调用者局部变量替代它。

`(let [name init ...] body...)` 与 `(let* [name init ...] body...)` **当前都按顺序绑定**：initializer 能引用此前绑定，但不能引用尚未建立的后续绑定；body 在新局部作用域内顺序执行。两者不是 parallel／sequential 的对立形式。

```lisp
(let [x 2 y (+ x 3)] (* x y))  ;; 预期 10；let* 同样成立
(def x 10)
(let [x 20] x)                 ;; 局部遮蔽，不改外层 x
```

绑定参数也可使用列表，但建议用向量；要求偶数个 name／init 项，name 为符号，不承诺解构语法。

`(def name expr)` 先求值 expr，再建立／替换**当前作用域**绑定并返回值；顶层 def 保留于当前会话，局部 def 不等于强制写入全局。`defn` 是当前作用域中命名函数定义。`(set! name expr)` 更新最近的已有绑定；若整条环境链没有该名称，则在当前作用域创建绑定，返回新值。闭包读写所捕获绑定的行为属于 AST／VM 对照合同，不能以两个实现使用不同存储机制解释差异。

同一局部作用域的后续 def／defn 可被先前闭包引用，但**预留不等于已执行**：定义之前有外层同名绑定时读外层值；没有则报告未绑定符号，而非返回伪造的 nil。quasiquote 中匹配深度的 unquote／splicing 会执行，内部定义参与当前作用域；未激活的模板数据不会建立绑定。

## 函数、rest 与尾位置

```lisp
(fn [x] (+ x 1))
(defn make-adder [k] (fn [x] (+ x k)))
(defn collect [first & remaining] [first remaining])
(collect 1 2 3)                ;; 预期 [1 (2 3)]
```

参数为符号列表／向量；`& rest-name` 收集多余参数为**列表**，包括空列表。固定参数要求精确个数；rest 函数至少需要固定参数个数。`fn`／`defn` 多个 body 表达式按 `do` 顺序执行；无 body 的 `fn` 返回 `nil`。函数是一等值，可传给 `map`、`reduce`、`apply` 等库函数；库函数签名另见库参考。

函数 body 的最后表达式、`do`／`let` 最后表达式、所选 `if`／`cond` 分支最后表达式以及 `and`／`or` 最后操作数可承接调用者尾位置。普通实参、测试条件和 initializer 不是尾位置。尾调用及其结果必须按语言控制流处理，而不是暴露内部控制标记。

`loop`／`recur` 提供显式重复：

```lisp
(loop [n 0 acc 0]
  (if (< n 5)
    (recur (+ n 1) (+ acc n))
    acc))                     ;; 预期 10
```

AST 与显式 VM 的 `loop` initializer 都在**外层环境**求值，不能将 let 的顺序绑定规则直接套给它；body 要求单个表达式，多表达式使用 `do`。`recur` 必须位于 loop 的尾位置，参数个数等于 loop 绑定数；参数按旧一轮绑定从左到右求值，再一起替换下一轮值。不承诺函数级 recur。上述边界由独立 AST／VM 合同核对，不使用回退 AST 的 `run_source` 代替编译执行。

## 宏、macroexpand 与 eval

过程宏 `(defmacro name [params...] body...)` 接收代码数据，在定义环境执行宏 body，返回可转回表达式的数据；展开结果在调用处求值。quasiquote 可构造展开结果。函数、对象、缓冲、future、channel 等值不能冒充源码形式。

```lisp
(defmacro twice [e] `(+ ~e ~e))
(macroexpand-1 '(twice 3))     ;; 预期 (+ 3 3)
(eval '(+ 1 2))               ;; 预期 3
```

`macroexpand-1` 只展开最外层宏调用一次；`macroexpand` 重复展开最外层直到不再是宏调用，**不递归遍历全部子表达式**。这两个函数与 `eval` 在语言会话的全局环境查找，不捕获调用点的词法局部绑定；需要的局部值应构造进数据或在被求值形式中显式绑定。

执行阶段有区别：AST 在求值到宏调用时展开；显式 `compile_source` 在编译阶段展开，`execute_module` 才运行普通表达式。宏的展开期 I/O 不属于运行期 I/O，不能要求它与前面的运行期 `println` 保持跨阶段时序；编译时需要的 helper 必须已在编译环境中可用，不能依赖尚未执行的普通顶层定义。L00 分别核对展开次数、展开结果与运行结果，不宣称任意带展开副作用的程序在两种入口中具有相同的跨阶段输出顺序。外部语言 `inline` 的阶段合同仍属于后续 E00。

现阶段显式编译拒绝依赖运行时局部捕获的宏声明；不能把 AST 可以捕获局部环境的过程宏，直接推断为编译期能够取得相同的运行时绑定。这个限制必须显式报错，不能用 `nil` 代替捕获值。

已有 `syntax-rules` 形式：

```lisp
(defmacro choose-first
  (syntax-rules ()
    ((_ x y) x)))
```

当前子集包含按顺序匹配的 pattern／template 子句、`_` 通配、符号／字面值、列表及定长向量模式、列表尾部 `...` 重复和对应模板替换。模板引入的 let／let*／loop 绑定、fn 参数及定义名称会与其引用一起改名。自由引用保持名称，仍在调用者环境解析；这不是完整 definition-site 标识符绑定模型。literal 匹配不是完整作用域感知标识符等价，不承诺任意嵌套 ellipsis、向量重复或完整 Scheme 卫生性。新宏应以已有子集的运行合同为边界。

## 模块、命名空间与授权

```lisp
(module :demo.math :export [double]
  (defn double [x] (* 2 x)))
(require :demo.math :as math :refer [double])
(double 3)
((get math :double) 3)
(demo.math/double 3)
```

module body 在新词法作用域执行；`:export [names...]` 及 body 内 `(export name...)` 声明公开的**模块自身绑定**。不存在的名称、仅继承自父环境的名称不能合法导出；空导出列表公开零个名称。`export` 不是隐式导出所有定义。

require 接受点分隔模块名（符号或关键字），已加载模块复用注册／缓存；未加载时通过宿主注册的 loader 读取。`:refer [names...]` 只导入声明导出的指定绑定；**不写 :refer 时默认导入全部 exports，即使写了 :as**。`:as alias` 额外建立关键字到导出值的映射，使用 `(get alias :name)`；不要把 alias 当作 `alias/name` 命名空间。真正的 `module.name/name` 在普通环境查找失败后从已加载模块的公开绑定解析，不负责隐式加载文件。

文件模块只在宿主授予的 roots 下解析；标准共享装配拒绝未授权文件模块、越界路径及循环加载，失败不能污染成功缓存。导入 Zio 库不会授予任意文件、原生引擎或浏览器能力。文件 I/O 与模块 roots 是分别由宿主配置的边界，不能把模块授权等同于所有 I/O 沙箱。

## ZOS 当前子集

已有类、实例、槽、通用函数与多参数分派；不宣称完整 CLOS／MOP。常用形状：

```lisp
(defclass Counter nil ((value :initarg :value :accessor counter-value)))
(defgeneric describe [x])
(defmethod describe ((x Counter)) (slot-value x :value))
(describe (make-instance Counter :value 7))
```

类支持单个父类或父类列表及当前 C3 线性化；槽定义是列表，记录 `:initarg`／`:accessor` 元数据，**当前不自动安装 accessor 函数**。实例构造当前按传入的关键字直接存槽，例子用同名 `:value`，不能据此宣称任意 initarg 到槽名的映射已实现。槽读取使用 `slot-value`。方法参数可为无约束符号或 `(name ClassName)`；支持 primary、`:before`、`:after`、`:around` 方法组合及方法中的 `call-next-method`。多语句方法 body 请用单个 `do`，不要假定它与 fn 的多 body 糖完全一致。类解析、继承槽、组合顺序与 next-method 都应依已有对象合同核对，不将规划的 eql／predicate specializer、完整 condition／restart、完整元对象协议写成已支持。`defpackage` 当前记录 package／use／export 信息，不替代 module 的加载和授权机制。

## 错误、try／catch 与 finally 边界

```lisp
(try (/ 1 0)
  (catch division-by-zero :caught))
(try missing
  (catch any *error*))
```

try 顺序执行 body；成功返回最后结果，失败转入匹配的尾部 catch。catch type 可为符号／关键字；类别包括 `symbol-not-found`、`type-error`、`wrong-arg-count`、`division-by-zero`、通用 `error`，`any` 匹配全部。handler 有局部 `*error*` 字符串，不是结构化异常对象；无匹配则传播原错误。当前 AST 从末尾收集 catch，因而多个匹配 catch 由**源码最后的匹配子句优先**；不要误以为按源码首个匹配。try body 当前不承接外层尾位置，匹配 handler 的最后表达式可承接。

**当前没有 finally 特殊子句。** `(finally ...)` 被当作普通表达式，通常报未绑定符号；它不保证清理资源。L00 合同必须记录此拒绝边界，不把设计中的 finally 目标当作能力；后续增加需要同时定义正常／异常／handler 失败下的次序与错误覆盖规则。

未捕获错误的名义类别包括：未绑定符号、不可调用值、参数个数（精确／至少／范围）、越界、类型错误、除零、非法形式、宏错误、非尾位置 recur、无 loop 的 recur，以及宿主／自定义错误。reader 类别包括 EOF、意外 token、奇数映射项、quote 缺失表达式和畸形字符串。入口可能将 reader 错误包装成求值错误；类别与原始载荷应保留为可核对合同，不只比较错误文本的一个偶然词句。

源码错误附带可用的 source span（字节区间、行列）及注册的 source name；跨文件 require 应保留实际失败文件来源。quote／宏生成的数据不自动拥有原源码的全部位置，不能伪造精确 span。错误类别、载荷、source name／span 与此前输出是四项独立观察，不能因为最终值一致就忽略它们。

显式编译产物携带可选原始源码版本；同名文件后来更新、产物经 JSON 迁移到新会话，都不能重定位已有函数／宏的错误。没有来源元数据的旧产物保留错误类别，但不借用当前同名文件伪造精确位置。

## I/O、同步占位与浏览器

`print`／`println` 对字符串输出原内容，参数间空格，println 加换行；`prn` 输出可读表示（字符串含引号／转义）。这些调用返回 `nil`。`read-line` 去掉行尾 CR／LF，读取失败当前返回 `nil`。`slurp`、`spit`、`load` 通过宿主 I/O；`load` 顺序求值全部形式并返回最后值。宿主能力的拒绝是错误，不是静默读取宿主任意路径。

`future-call` **立即同步执行**零参数函数并包装结果，错误在调用点传播；无后台线程。promise 经 deliver 后可 deref，未交付的 promise 当前快速报错而非等待。channel 是同步宿主消息队列；空队列 recv 与满的有界队列 send 可以阻塞当前执行线程。不能以存在 future／chan 名称推断并发调度、取消、安全隔离或不会阻塞。

browser WASM 会话保留自己的语言环境；输出走浏览器 console，不是原生 stdout。它没有 stdin／工作目录／文件写入能力，文件读取限已注册虚拟源码；当前 `read-line` 将缺少 stdin 的失败变为 `nil`。浏览器提供 `js/` bridge、对象句柄及解释器 dotted resolver；原生 CLI 不因此获得 JS 支持，编译路径与浏览器解释路径也不能无证据视为一致。DOM、fetch、计时器等能力属于浏览器宿主，不属于 Zio 核心求值规则。

## 名义语义类别与可运行合同

L00 `langs/core/tests/toolchain_contract.rs` 包含 26 项独立 AST／VM 对照和 2 项 VM／产物合同；名称用于索引，执行证据见本节末尾：

| 类别／测试名 | 对应公开合同 |
| --- | --- |
| `lexical_bindings_and_captured_mutation` | let／let* 顺序、遮蔽、set!、捕获变异 |
| `call_arguments_are_left_to_right` | 实参次序及记录输出 |
| `truthiness_and_short_circuit` | nil／false、and／or |
| `rest_higher_order_and_closures` | rest 列表、闭包、HOF |
| `loop_recur_and_tail_calls` | loop initializer、recur、尾调用 |
| `quote_and_nested_quasiquote` | 引用、嵌套深度及列表／向量／映射模板 |
| `procedural_macros_expand_once` | 原始宏参数、单次展开与运行期副作用 |
| `syntax_rules_supported_subset` | literal、尾部 ellipsis 与模板替换 |
| `try_catch_and_finally` | catch、handler、finally 未支持边界 |
| `modules_export_qualified_refer_and_isolation` | 导出隔离、qualified／refer、加载缓存 |
| `zos_dispatch_and_next_method` | 对象、分派、next-method 子集 |
| `integer_precision_and_division` | 整数／浮点类别、精确比较与截断除法 |
| `runtime_errors_preserve_payload_and_location` | 求值错误类别、载荷、来源及输出 |
| `reader_errors_preserve_category_and_location` | 读取失败与来源 |
| `conditional_and_local_definition_results` | cond 列表子句、无命中、局部 def 与遮蔽 |
| `inline_module_and_alias_maps_preserve_exports` | 内联 Zio module、as 映射与作用域隔离；不是外语 inline |
| `errors_inside_functions_keep_definition_locations` | 函数 body 的错误来源不能变成 caller 来源 |
| `nested_handlers_preserve_outer_error_binding` | handler 错误传播、catch 优先级与 *error* 作用域 |
| `macro_effects_obey_entry_phase` | AST 即时展开与显式编译期／运行期的输出边界 |
| `mutual_tail_calls_and_variadic_rebinding` | 跨函数尾调用及 variadic frame 重绑定 |
| `local_definitions_shadow_outer_bindings` | 局部 def／defn 不修改外层绑定；定义执行前仍可查找外层值 |
| `local_forward_definitions_share_bindings` | 同一作用域的前向定义、相互尾递归，以及匹配深度 unquote 内定义 |
| `forward_bindings_remain_unbound_until_definition` | 预留绑定不等于已定义；无外层值时保留未绑定符号错误 |
| `compiled_errors_keep_the_source_revision` | 同名源码更新与产物迁移不能改变原错误来源 |
| `legacy_artifact_does_not_guess_error_source` | 无来源元数据的旧产物不借用当前同名文件的位置 |
| `compiled_macro_errors_keep_definition_source` | 编译宏的 body 错误保留定义版本，而非调用版本 |
| `vm_non_tail_calls_return_and_unwind_without_rust_recursion` | VM 1000 层普通调用返回，以及错误跨调用帧回到 catch |
| `equal_datum_syntax_keys_compile_deterministically` | 同 datum、不同源码位置的 map 语法键：跨 fresh context 产物字节一致，编译执行结果一致 |

独立执行面必须这样建立：每个 case 的每个面使用 fresh `language_context_with_io` 与 `BufferIoHost`，roots 用 `new_with_io` 并共享该面宿主；AST 调用 `bootstrap::eval_source`，VM 调用 `bytecode::compile_source` 后 `bytecode::execute_module`，元循环调用 `interpreter::eval_source`。三面不能复用先前定义或模块缓存；值／结构化错误类别及载荷／source span 与 name／输出分别比较。`bytecode::run_source` 当前顶层仍经过 AST，**不能代替显式 VM 对照**。

L00 历史验收执行 `cargo test -p zio-core -p zio-cli`：238 项通过，包括当时的 28 项 toolchain、24 项 runtime、3 项 staged compiler 合同。Stage 1／2 编译产物规范 JSON 字节一致；Stage 2 安装整个编译模块及其辅助函数，不只替换入口。此固定点仍依赖 Rust reader／宿主机制，**不是**整门语言自举。可运行基线为 `examples/toolchain/semantics.zio`；L00 原生源码与 fresh-context 便携 VM 产物都验证了同样的 16 行输出及 nil 返回。

L01 最终 `cargo test -p zio-core -p zio-cli --no-fail-fast`：**258 项通过，0 失败**，其中 toolchain 29 项、元循环 17 项、syntax atom 2 项。元循环深度合同覆盖 10000 次尾调用、1000 层普通返回／错误回退及递归 map／reduce、3000 次尾部 apply；函数、回调、宏、ZOS 方法、模块初始化有禁止目标 Stage 0 AST 回退的运行检查。完整证据与保留边界见主计划 L01。

L01 WASM 已从当前核心重建，并在 Node 中调用实际 `ZioSession`：元循环执行同一基线返回 nil、console 精确记录 16 项输出；1000 层递归 map 返回 1000；包声明 `::use`／`::export` 与 AST 得到相同的 `[:core :answer]`；文件写入因浏览器宿主无权限被拒绝。原生 `meta-eval.zio` 示例也实跑输出相同 16 行。这是实际引擎验收，不是浏览器 DOM／视觉验收。

L02 实跑 `cargo test -p zio-core -p zio-cli --no-fail-fast`：**259 项通过，0 失败**。
CLI smoke 写出的 Stage 1/2/3 均为 739890 字节，文件 `cmp` 一致；
fresh 编译前端得到 sum=10、closure=15。合同覆盖 UTF-8 原始源码位置、
宏错误定义来源、坏版本/指令/常量拒绝、缺失 helper 禁止 Stage0 回退及保护回调身份。
重建 WASM 后，Node 实际 `ZioSession` 的 Zio reader/compiler 安装并执行编译宏，
得到同样的 sum/closure；元循环 1000 次尾调用返回 1000。Astro 构建 88 页，
HTTP 核对当前文档/状态和 WASM 字节，浏览器 DOM/截图核对自举命令段落渲染；
该文档视觉检查不冒充浏览器完整编译器自举或应用导出验收。
