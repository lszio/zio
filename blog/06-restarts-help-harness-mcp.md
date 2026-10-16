# 让 Agent 进入语言运行时：从 Common Lisp restart 到 help、harness 与 MCP

更新：2026-10-16。这篇文章区分已经落地的查询与仍在设计的恢复、harness、MCP。ZIO-003/004 已提供查询合同、native CLI/REPL 实现与行为验收；condition/restart、执行 harness 和 MCP API 仍为规划。详细结构见[AI 交互设计 §4](../docs/ai-language-interaction.md#4-查询合同-v1zio-003)，当前证据见[特性矩阵](../docs/feature-matrix.md)。

一个新语言想让 AI 用起来，除了让模型能写出正确括号，还需要回答几个实际问题：它怎样找到可用函数，怎样确认某项特性真的存在，怎样运行实验，以及程序出问题后怎样继续？

Zio 可以把这些问题纳入语言运行时。人类和 Agent 使用同一套查询与恢复能力，Agent 只是多了一种外部客户端。

## 1. catch 与 restart 解决的问题不同

考虑处理一批记录：已经成功写出前九条，第十条缺一个数值。若错误一路传播到最外层 catch，底层调用通常已退出。Agent 得到错误文本后修改文件并重跑，可能再次写出前九条。

Common Lisp 将问题拆开：condition 描述发生了什么；handler 决定如何应对；restart 由程序在具体位置提供恢复方式。handler 可以在原动态环境仍有效时作选择，不必先把整个失败调用退栈。这是 [handler-bind](https://www.lispworks.com/documentation/HyperSpec/Body/m_handle.htm) 所支持的机制。

例如程序可以在缺值的位置主动提供“使用指定值”和“放弃本次处理”。这里的 restart 是有明确语义的恢复入口，不是重启操作系统，也不是把所有代码再执行一次。

下面是原创的 Common Lisp 教学例子，本轮未运行，**不能直接当作 Zio 代码**：

```lisp
(defun read-count ()
  (restart-case
      (error 'type-error :datum "unknown" :expected-type 'integer)
    (use-value (value)
      (check-type value integer)
      value)))

(handler-bind
    ((type-error
       (lambda (condition)
         (declare (ignore condition))
         (invoke-restart 'use-value 21))))
  (* 2 (read-count)))
```

按这些规则，handler 选择 use-value 并传入 21，read-count 返回替代值，外围计算得到 42。实际使用应选择适用的 restart，并验证恢复参数；例子只为说明控制路径。

这里仍存在退栈：`restart-case` 转移到所选子句前，会退去它与信号位置之间的动态帧。关键是 handler 在这些恢复点仍有效时作出选择，外部计算可以继续；不能把它描述为“完全不退任何栈”。[CL restart-case](https://www.lispworks.com/documentation/HyperSpec/Body/m_rst_ca.htm)

## 2. 对 Agent 有价值的是可查询的选择

普通错误文本告诉模型“失败了”，结构化 condition 可以给出类别、输入、来源与相关调用。恢复列表进一步告诉它：这次失败具体允许做什么，参数是什么，会改变什么。

同一恢复名称可能出现多次。机器接口应使用这次执行中的实例 ID，而不是看到 `retry` 这个词就随便调用。Common Lisp 的 [compute-restarts](https://www.lispworks.com/documentation/HyperSpec/Body/f_comp_1.htm)本就根据动态状态列举适用恢复点；为其增加参数 schema、效果说明和外部句柄，是 Zio 的设计建议。

restart 只能表达程序已经提供的恢复动作。把非法的除法随意改成 0，或者绕过授权失败，都不能因为“AI 自己决定”而成为正确恢复。某个程序没有可用恢复点时，Agent 仍需要终止该次计算、检查来源或重新修改代码。

## 3. help 应当能被程序读取

终端上的漂亮帮助文本适合人读，但 Agent 还需要知道：这是函数还是宏？参数怎样传？在 WASI 上能否运行？是否需要文件权限？示例与当前版本是否一致？

现在 `describe-symbol` 返回结构化数据，`help` 从同一个描述经 IoHost 输出人可读文本并返回 nil；`apropos` 负责搜索，`language-capabilities` 只报告已声明的能力：

```lisp
;; 当前查询接口；运行完整例子：examples/toolchain/query.zio
(help 'map)
(describe-symbol 'map)
(apropos "collection")
(language-capabilities)
```

文档应沿用[源码注释](../docs/libdoc-convention.md)，关联真正的符号身份、模块版本和源码修订，而不是另写一份“给 AI 的帮助”然后逐渐过期。动态函数无法静态证明的返回类型或效果，明确标未知。

查询不应顺手执行函数、getter、宏或文档示例。否则“了解这个库”本身就会改变会话。想试用示例，要显式进入执行 harness。

内部不另建一份“给 Agent 的文档”：查询读取当前根环境绑定，从 callable 取得参数，
从其原始 SourceMap 修订提取相邻注释；native 文档随注册的函数身份携带。
所以保存旧函数的 alias 后重新定义同名函数，旧 alias 仍指向旧文档，不按文件名
借用最新内容。宏、模块导出、函数与缺失文档保持统一的描述结构。

这里有明确代价：查询只覆盖根环境；没有可信 span 的生成/旧代码会返回未知，
未标注的 native 不会被写成“零参数”，也不根据查询结果授予文件或网络权限。
`language-capabilities` 的 `remote-restart` 仍是 false；下面的执行会话和 MCP
不是这批查询接口顺带实现的功能。


## 4. harness 是 Agent 的语言实验环境

可以把语言执行 harness 理解为管理试验的实验台：它知道使用哪个语言版本、模块和执行器，记录源码与结果，捕获输出，限制计算量，并管理会话。编译器、VM 和 GC 仍各自负责底层工作。

例如 Agent 查询一个函数后，创建干净会话，编译一个小程序，运行并比较结果。如果发现目标不支持某项能力，它应得到结构化拒绝；如果遇到可恢复 condition，可以选择恢复点。已经授予的工作区和预算内允许反复实验，不需要每轮都重新征求许可。

已有 Loom harness 管理模型对话、工具、预算和取消；它可以消费语言执行 harness，但两者职责不同。解释器向外提供实验能力，不应该先要求自己绑定一个模型供应商。

实验记录也不等于世界可重放。时间、随机数和网络结果需要记录或注入；文件已经写入后，取消不会自动撤回。试验、验证、采用某个结果应有明确状态。

## 5. 解释器完全可以成为 MCP server

候选入口是 `zio mcp --transport stdio`：外部 Agent 通过 MCP 查询、编译和运行 Zio，同一解释器服务保持显式的 Zio 会话。普通用户仍可通过 REPL 和 CLI 使用同样能力。该命令目前是设计目标。

MCP 提供工具/资源的协议形式，Zio 负责语言语义和执行状态。比如提供 query、compile、run、inspect、restart、cancel 等稳定工具，再把函数和恢复点作为数据查询；不必把成千上万个语言函数都变成 MCP tools。[MCP tools 规范](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)

需要区分应用会话与连接。参考的 2026-07-28 版 MCP 不以协议连接承诺会话连续性，因此 session_id/run_id 由 Zio 显式返回和接收。这样也能防止同一连接上的不同任务串用环境；老版本客户端需独立兼容适配。[MCP basic](https://modelcontextprotocol.io/specification/2026-07-28/basic)

stdio 模式下，stdout 是协议通道。程序中的 println 必须捕获到工具结果，不能直接夹进 MCP JSON 消息；日志可用 stderr。[MCP stdio](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio)

## 6. 最难的部分：Agent 想一会儿，程序要留在哪里

语言内的同步 handler 可以立即调用 restart。但远程 Agent 通常要先读结果、再发起下一次工具调用。此时运行时必须真的保留暂停帧、局部变量与动态恢复链，才能继续同一次计算。

如果先把错误变成字符串返回，调用已经结束，再给 Agent 一个“继续”按钮，底层只能重跑；这会丢失现场并可能重复副作用。MCP 本身不会自动变出续体。

正确的暂停结果应含 run ID、暂停代际、condition 和有效恢复实例。下一次请求携带对应 ID 与参数；过期、跨会话或已消费的恢复点拒绝。网络重复请求也不能让同一个恢复动作执行两次。

等待期间，暂停帧必须被 GC 当作仍有效的 roots。Agent 取消、会话关闭或租约过期后，运行时才按合同退出和清理。当前静止镜像不保存这类在途调用栈，也不能用来直接实现它。相关基础可先读[编译器、VM 与 GC 入门](05-compiler-vm-gc.md)。

## 7. 一条可以逐步验收的路径

先做结构化查询与语言内 condition/restart；建立能编译、运行、观察和取消的实验 harness；以 MCP 接入现有可用执行面，明确暂不提供的能力；最后在可暂停 VM 与 GC roots 具备后接通跨工具调用的 restart。

首个完整演示应让 Agent 从未知函数开始，查文档、写小实验、触发一个明确缺值 condition、选择 use-value，然后完成原来的计算。验收时还应观察前置副作用没有重复、旧恢复句柄不能再用，以及取消后资源得到清理。

这样的竞争力可以测量：任务完成率、无效调用次数、恢复后副作用、所用时间与 token。相比只让语言“长得容易生成”，这条闭环让 Agent 能通过真实运行确认自己理解了什么。
