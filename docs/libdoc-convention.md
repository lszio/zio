# 库文档注释约定

`libs/` 下的 Zio 源码使用注释承载库文档。站点的库参考页面
（`/reference/`）由 `apps/site/src/lib/zio-lib-docs.mjs` 在构建时从这些
注释直接提取生成——**源码注释是唯一的文档来源**，不维护第二份文档。

## 规则

1. **文件头**：文件顶部连续的 `;;` 块是文件文档；第一行为标题（形如
   `;; ── zio.vector — 语义向量索引 ──` 或 `;; 标题 — 说明`），其余为描述。
2. **分节**：`;; ── 节名 ──────` 横幅行开启一个分节，其后直到下一个横幅
   之间的顶层表单都归入该节。横幅只作分组标记，不进入正文。
3. **符号文档**：紧跟在顶层表单上方（中间无空行）的连续 `;;` / `;; doc:`
   块是该符号的文档。格式：

   ```lisp
   ;; doc: (fn-name x) — one-line summary.
   ;; doc:
   ;; doc: Details: parameters, semantics, return value, edge cases.
   ;; doc:
   ;; doc:   indented lines render as an example code block
   ;; doc:   (get #{:a} :a)
   (defn fn-name [x] ...)
   ```

   两种前缀等价：`;; doc: …`（推荐，便于 grep）与普通 `;; …`。
   首行写 `` ;; doc: `name` — summary `` 或 `;; doc: (name args) — summary`；
   语言跟随该文件既有文档风格。
4. **代码示例**：doc 行内缩进 ≥2 格的行按顺序合并为一个 `zio` 代码块。
5. **归属判定**：doc 块与表单之间出现空行即视为与该表单无关（属于上文
   的段落说明）。因此文档必须紧邻表单。

## 提取的表单

`def`、`defn`、`defmacro`、`defstruct`、`defprotocol`、`defentity`、
`defclass`、`defgeneric`、`defmethod` 的顶层（列 0）表单。

## 验证

```sh
cd apps/site && bun run build
```

构建后 `dist/reference/` 应包含每个 `libs/**/*.zio` 文件一页。
