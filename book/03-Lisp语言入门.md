# 03 - Lisp 入门：S 表达式、求值、函数

## 一切从括号开始

Lisp 最显著的特征：**到处是括号**。但这不是为了奇怪——括号统一了"代码的结构"和"数据的结构"。

Zio 中每个表达式都是 **S 表达式**（Symbolic Expression）：

```
(运算符 参数1 参数2 ...)
```

这就是全部语法。没有运算符优先级、没有分号、没有大括号。

## 基本类型

在 REPL 中试试：

```lisp
42              ;; → 42          整数
3.14            ;; → 3.14        浮点数
"hello"         ;; → "hello"     字符串
true            ;; → true        布尔值
nil             ;; → nil         空/假
:symbol         ;; → :symbol     关键字（类似枚举标签）
foo             ;; → 查找变量 foo 的值
```

## 算术：你的第一个程序

```lisp
(+ 1 2)          ;; → 3
(* 2 3)          ;; → 6
(- 10 4)         ;; → 6
(/ 15 3)         ;; → 5
```

**注意**：运算符写在第一个位置，不是中间。`1 + 2` 在 Lisp 中是 `(+ 1 2)`。

Zio 支持多个参数：

```lisp
(+ 1 2 3 4 5)    ;; → 15  所有数加起来
(* 2 3 4)        ;; → 24  所有数乘起来
(= 3 3)          ;; → true  等于
(< 2 5)          ;; → true  小于
(> 5 2)          ;; → true  大于
```

## 数据容器

### 列表（List）

```lisp
'(1 2 3)         ;; → (1 2 3)           一个列表
(list 1 2 3)     ;; → (1 2 3)           用函数创建列表
(cons 1 '(2 3))  ;; → (1 2 3)           从头部添加元素
(car '(1 2 3))   ;; → 1                 取第一个元素
(cdr '(1 2 3))   ;; → (2 3)             取剩余元素
```

`'` 是 **quote** 的缩写——意思是"不要求值"。没有 `'`，`(1 2 3)` 会被解释为函数调用：把 `1` 当函数调用，传入 `2` 和 `3`。

### 向量（Vector）

```lisp
[1 2 3]          ;; → [1 2 3]           向量——类似数组
(vector 1 2 3)   ;; → [1 2 3]           用函数创建
```

列表和向量的区别：列表适合从头添加/移除元素；向量适合按索引访问。

### 映射（Map / 字典）

```lisp
{:a 1 :b 2}      ;; → {:a 1 :b 2}      键值对映射
(get {:a 1 :b 2} :a)  ;; → 1           查值
(put {:a 1} :b 2)     ;; → {:a 1 :b 2}  添加键值对
```

## 定义变量

```lisp
(def x 42)       ;; 定义全局变量 x 为 42
x                ;; → 42
(def pi 3.14)    ;; 定义 π
(* pi 2)         ;; → 6.28
```

`def` 创建的变量在当前环境可见（类似于全局变量）。

## 局部绑定

```lisp
(let [x 10
      y 20]
  (+ x y))       ;; → 30
```

`let` 创建**临时**的局部绑定。x 和 y 只在 let 内部有效。

`let*` 允许后面的绑定引用前面的：

```lisp
(let* [x 10
       y (* x 2)]
  (+ x y))       ;; → 30   y 可以引用 x
```

## 定义函数

```lisp
(defn square [x]
  (* x x))

(square 5)       ;; → 25

(defn add [a b]
  (+ a b))

(add 10 20)      ;; → 30
```

`defn` = `def` (定义) + `fn` (函数)。`[x]` 是参数列表。

### 匿名函数

```lisp
(fn [x] (* x x))     ;; → #<function (x)>
((fn [x] (* x x)) 5) ;; → 25   直接调用
```

### 递归函数

```lisp
(defn factorial [n]
  (if (<= n 1)
    1
    (* n (factorial (- n 1)))))

(factorial 5)    ;; → 120
```

## 条件判断

```lisp
(if (> 3 2)
  "yes"
  "no")          ;; → "yes"
```

`if` 的格式：`(if 条件 真分支 假分支)`。

多分支用 `cond`：

```lisp
(defn classify [x]
  (cond
    (< x 0)  "negative"
    (= x 0)  "zero"
    :else    "positive"))

(classify -5)    ;; → "negative"
(classify 0)     ;; → "zero"
(classify 42)    ;; → "positive"
```

## 顺序执行

```lisp
(do
  (println "step 1")
  (println "step 2")
  (+ 1 2))       ;; → 3   返回最后一个表达式的值
```

## 循环：用递归不用 for

Lisp 没有 `for` 循环。使用递归或 `loop/recur`：

```lisp
;; 递归方式
(defn countdown [n]
  (if (= n 0)
    "done"
    (do
      (println n)
      (countdown (- n 1)))))

;; 尾递归优化（loop/recur）
(loop [n 10]
  (if (= n 0)
    "done"
    (do
      (println n)
      (recur (- n 1)))))
```

`recur` 跳回到最近的 `loop` 重新执行——且**不消耗栈空间**（尾调用优化）。

## ⚡ 动手实验

在 REPL 中逐行输入：

```lisp
;; 1. 基本算术
(+ 1 2 3 4 5 6 7 8 9 10)

;; 2. 嵌套表达式
(* (+ 2 3) (- 10 4))

;; 3. 组合函数
(defn celsius-to-fahrenheit [c]
  (+ (* c 1.8) 32))

(celsius-to-fahrenheit 100)  ;; → 212.0

;; 4. 判断奇偶
(defn even? [n] (= 0 (mod n 2)))
(defn odd? [n] (not (even? n)))

(even? 10)  ;; → true
(odd? 10)   ;; → false

;; 5. 高阶函数
(def numbers [1 2 3 4 5])
(map (fn [x] (* x 2)) numbers)      ;; → (2 4 6 8 10)
(filter (fn [x] (> x 2)) numbers)   ;; → (3 4 5)
(reduce + 0 numbers)                ;; → 15
```

## 你学到的核心概念

| 概念 | 一句话 |
|------|--------|
| S 表达式 | `(操作符 参数 ...)`——全部代码的统一结构 |
| 求值 | eval 把代码变成值；符号去环境查找，列表去调用函数 |
| quote `'` | 阻止求值，把代码当数据 |
| 函数 | 一等公民：可以定义、传递、作为返回值 |
| 递归 | Lisp 的默认循环方式 |

## 对应源码

- `sexp.rs` —— S 表达式（语法树）的类型定义
- `eval.rs` —— eval/apply 求值器

## 下一章

我们深入到"代码"和"数据"是如何在 Zio 内部表示的。
