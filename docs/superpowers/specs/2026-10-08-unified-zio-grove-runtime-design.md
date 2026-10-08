# Zio 站点与 Grove 统一运行、dev 和 MR 预览设计

**状态：** 用户已选择“一个 Dokploy Compose 栈、两个服务”；本文档等待用户审阅。本文是设计，不代表功能已实现或获准部署生产。

## 1. 目标与边界

一个版本提交同时构建并运行 Zio 静态站点与 Grove 应用；`main`、`dev` 和每个获准的 MR 使用相同运行结构。Grove 页面与 API 保持同源，生产、开发、MR 预览的数据和令牌相互隔离。

统一是**一个环境一个 Compose 部署单元**，不是把 Nginx、站点和 Grove 队列合并为一个进程。内部服务边界保留，统一由一个 Nginx 入口和一套版本提交协调。

本设计选择仓库声明的 canonical Grove 入口：`tools/install.sh` 生成的 `bin/grove` → `bin/zio` → `apps/grove/main.zio`。不得为了上线绕过它，静默改用 `apps/grove/native/app` 中的旧 Rust 服务。Rust 业务/宿主仍是迁移过程中的现有实现；迁移状态以 ADR-019 与 Grove convergence plan 为准。

本文只界定部署与环境边界，不缩减 Grove 已批准的产品范围。统一运行单元承载 Grove 应用及其教师/反馈、训练、检查点与站点审查能力；ACP 双向接入等仍按 ADR-019 和 convergence plan 验收，本设计不宣称其已实现。当前服务必须达到真实 HTTP/队列/worker 合同；只返回 `queued`、健康页或占位评价不算 Grove 已运行。

Grove 镜像包含应用入口与所需 Zio 库；Numa/Rill/Loom 是进程内库依赖，不新增 Compose 服务。运行时须承载 Zio 源码、向量/神经网络/LLM 等可插拔模块、上游模型教师与人类反馈；ACP 按 ADR-019 先客户端/教师适配、后服务端。模块、权重和 checkpoints 存于独立 artifact root，并沿用能力声明、状态兼容、冻结区和人工发布边界；本部署设计不把这些能力宣称为已实现。

## 2. 现状与原因

| 观察 | 证据 | 影响 |
|---|---|---|
| 生产 `zio` 跟踪 `main`，自动部署到 `zio.lszio.space`；最近部署为 2026-10-04 17:17 UTC | Dokploy `compose.one` / `domain.one` | PR #3 合并到 `dev` 不会更新生产域名，这是当前分支配置的预期结果。 |
| `zio-dev` 跟踪 `dev`，自动部署到 `zio-dev.lszio.space`；2026-10-08 06:25:55–06:26:50 UTC 有完成部署记录 | Dokploy `compose.one` / `domain.one`；PR #3 合并于 06:25:51 UTC | dev 环境已经存在；无需再创建第二套。部署记录未提供 commit hash，因此仅能确认时间和分支配置相符。 |
| 两个 Dokploy 项目都是 Compose，`triggerType=push`；当前根 Compose 只暴露一个 Nginx 服务 | Dokploy 查询；`docker-compose.dokploy.yml:1-8` | 当前没有每 MR 的 Compose 预览。 |
| 根 `Dockerfile` 只构建 Astro 并以 Nginx 服务静态内容 | `Dockerfile:1-24` | 当前线上镜像不包含 Grove 服务进程。 |
| Astro 使用根路径 `/`，并拥有 `/grove/` 介绍页 | `apps/site/astro.config.mjs:7-13`；`README.md:78-96` | Grove 产品页面不能直接占用 `/`；介绍页仍保留。 |
| 安装产品的实际入口是 Zio launcher 与 `apps/grove/main.zio` | `tools/install.sh:30-43,73-109`；`apps/grove/main.zio:178-221` | 部署必须按 Zio app 契约打包，不能把 `grove-app` Rust binary 误认成该入口。 |
| Zio HTTP 服务以相对路径读取 Grove UI 和 WASM 资产；Host `read-bytes` 将相对路径解析到进程 CWD 并按授权读根检查 | `apps/grove/api.zio:534-553,629-659`；`contribs/native/host/src/storage_fs.rs:132-155,238-240`；`tools/install.sh:49-59` | 安装器打包 Grove 源码但不打包 `apps/site` 的 WASM；当前读取依赖运行目录，独立镜像需显式资产根。 |
| Zio `api--serve` 以 reader 身份打开 store，且 HTTP loop 未启动 `runner--open` / `runner--tick` | `apps/grove/api.zio:376-400,629-659`；`apps/grove/store.zio:152-169`；`apps/grove/runner.zio:40-44,181-204` | 目前不能将 Zio HTTP server 视为可执行学习队列的完整服务。部署前必须接通可写 store 和唯一 owner loop。 |
| 部分 Zio 产品 handler 仍是占位或未接通行为 | `apps/grove/main.zio:165-172`；`apps/grove/api.zio:345-374`；`docs/adrs.md:1000-1006` | 不能以 API 可达、HTTP 202/queued 回执或占位评价作为产品功能验收。实现范围须与现有 Grove 计划的实际验收对齐。 |
| GitHub CI 的 push 触发器只包含 `main`，另有所有 PR 的检查 | `.github/workflows/ci.yml:3-7` | `dev` 的合并后 push 没有独立 CI 运行；需要 `dev` push 检查及分支保护。 |
| CPU worker 依赖 Linux namespace jail；测试环境缺少 namespaces 时，相关真实训练测试会显式 skip | `contribs/native/host/src/transport/isolation.rs`；`apps/grove/native/app/tests/runner_contract.rs:372-374,515-517`；`apps/grove/native/learning/tests/isolation_contract.rs:145-148` | Dokploy 目标容器的能力仍未知。不得通过 `privileged: true` 或关闭隔离来制造通过。 |

## 3. 部署模型

每套环境使用同一 Compose 文件、同一版本提交和两个内部服务：

1. **`site`**：Astro build + Nginx。Nginx 服务 `/`、Astro 产物、docs/book/blog、examples 与 `/wasm/*`；只对 Dokploy 暴露 80 端口。
2. **`grove`**：canonical Zio launcher/HTTP app，内部监听 `0.0.0.0:8787`，不发布宿主端口。它拥有自己的持久数据根和所需 CPU worker 运行时。

保留现有 Dokploy `zio`/`zio-dev` Compose 项目 ID、分支和域名；将域名 upstream 改为新的 Nginx service `site:80`，不重复创建生产或 dev 栈。

Nginx 路由保持一个公共 origin：

- `/`：Zio Astro 站点。
- `/grove/`：保留现有 Grove 产品介绍页，并提供进入 `/grove/app/` 的明确入口。
- `/grove/app/`：Grove 控制面；Nginx 去掉此前缀后转发到 Grove 服务的 `/` 与 UI 静态资源。
- `/api/*`：转发至 Grove 服务的相同 `/api/*` 路由。浏览器和 API 仍是同 origin，不需要放开 CORS。
- Grove `index.html` 当前将 bridge 指向 `/bridge.js`，需改为 `/grove/app/bridge.js`；`styles.css` 和 `controller-source.js` 保持相对 URL。API 继续使用 `/api/*`，WASM 继续使用 `/wasm/*` 并由站点直接提供。
- 切换完成后，删除 Grove API 的 `/wasm/*` routes、public-path 白名单和 handler/资源读取；该路径由 `site` 服务，避免保留不可达的第二份实现。

Grove 的内部 HTTP 端口不得添加单独的公网 router。`/grove/app` 应规范化为 `/grove/app/`。预览与正式环境使用完全相同的 Nginx 路由规则。

## 4. Grove 服务运行合同

在构建容器前，canonical Zio 服务必须满足以下运行不变量：

- 首次启动可在已授权的数据根创建 schema；服务用可写 store 句柄处理变更，HTTP 请求按各自角色做授权，不能以 reader SQLite 连接承载写操作。
- 每个环境的 Grove 服务单实例运行并保持唯一活跃 coordinator epoch；`serve` 必须构造并持续驱动一个 owner，令队列从 `queued` 进入实际执行、持久化进度和终态。请求返回 run ID 不足以证明工作已执行。
- HTTP 等待不能阻塞 owner 调度；并发与轮询边界必须可观察。服务重启时按现有 epoch、lease、checkpoint 合同接管，不接受旧 owner 的回执。
- 资产路径从显式安装根解析，不依赖 checkout、当前工作目录或任意的 `ZIO_PATH`。`site` 镜像包含 Astro 产物和 WASM；`grove` 镜像包含 Grove UI；各自的 host 读根精确授权对应资产。
- Grove 外网 bind 必须有各环境独立角色令牌；reader、annotator、operator、publisher 的授权边界不能合并或跨环境复用。
- 可信 runtime/core/evaluator/authorization 逻辑随镜像只读交付；候选 Zio 模块、权重和 checkpoints 写入分离的受控数据根，宿主冻结区与人工发布门槛不因容器配置而放宽。
- 若启用 Torch worker，镜像提供锁定的 CPU 依赖；隔离 worker 使用受限文件挂载、独立 scratch、CPU/内存/时限配额和现有 jail。隔离探测失败时拒绝训练，不回退到不受限执行。

现有 `apps/grove/runner.zio` 定义 owner 操作，但当前 `api--serve` 没有调用它；此接线及配套真实行为属于部署前置，不是允许部署时掩盖的待办。Rust `grove-app` 中的 owner loop 是迁移期参考，不自动成为 canonical Zio runtime。

## 5. 环境与数据边界

| 环境 | Git ref | 域名 | 生命周期与数据 |
|---|---|---|---|
| Production | `main` | `zio.lszio.space` | 持久 Grove 数据根并有经验证的备份/恢复；仅 production 令牌和配置；不得被 `dev` 或 MR 更新。 |
| Dev | `dev` | `zio-dev.lszio.space` | 独立持久数据根和独立令牌；复用现有 Dokploy `zio-dev` Compose 项目。 |
| MR preview | 目标为 `dev` 的获准 PR head commit | `zio-pr-<number>.<preview-zone>` | 每 PR 一个临时 Compose 栈和专属数据库/制品 volume、短期 preview 令牌及受限资源配额；PR 关闭或合并时显式删除栈、域名和 volume。 |

预览不得挂载、读取或复制 production/dev store、令牌、权重或持久卷。需要外部教师时，只能使用隔离的测试凭据与独立额度；不得复用 production/dev 凭据，也不得以 mock/echo 代替教师行为。生产发布继续只由 `main` 控制；合并至 `dev` 不自动提升到生产。

预览域名的 wildcard DNS / 证书当前未验证。实施前使用实际 HTTPS 请求确认 wildcard 路由，或使用 Dokploy 提供的动态域名；不得仅凭本机 `dig`/`getent` 结果宣称 DNS 已就绪。

## 6. CI 与 MR 生命周期

- CI push 触发覆盖 `main` 与 `dev`；PR 检查覆盖这两个目标分支。分支保护要求对应检查通过并禁止普通直接推送；Dokploy 仍由 push 自动部署，因此合并前 PR 检查是部署门槛，合并后的 push CI 不能单独阻止已触发的部署。
- CI 必须从同一 commit 构建 `site` 与 `grove` 两个服务镜像。除 workspace/格式检查外，还要验证镜像启动、站点路由、Grove UI/API 同源路由、角色授权、可写 store、真实 owner 执行和重启后的持久记录。
- CPU 训练验收必须在具备真实 namespace/jail 能力的环境运行。受限 runner 的 skip 不是训练支持的证明；若 GitHub-hosted runner 无法提供该能力，使用受控 self-hosted runner 或在目标 dev 容器做受审的集成验收。
- Dokploy Compose 没有当前应用可直接打开的原生 PR preview；官方预览功能文档针对 Application。为保留 Compose 两服务边界，PR lifecycle 由受限的 GitHub workflow 调用 Dokploy Compose API：获准 PR 打开/重新打开/更新时创建或更新对应栈，关闭/合并时删除。
- 含 Dokploy 凭据的 workflow 不 checkout 或执行 PR head 脚本；仅允许同仓库且有写权限的贡献者触发部署，或者要求维护者显式批准。Dokploy 会构建 PR 提交，所以这一权限门是执行不可信 Dockerfile 的安全边界。
- PR preview 的 Compose 栈只在检查通过后部署；更新、关闭或合并时显式清理服务、域名和专属 volume。创建、更新或清理失败必须让对应 workflow 呈失败，不留下“可预览”的假状态。部署记录关联 PR 编号、head SHA、URL 和清理状态。

## 7. 方案比较

| 方案 | 优点 | 代价/风险 | 决定 |
|---|---|---|---|
| 一个 Compose 栈、`site`/`grove` 两个服务、同源 path routing | 一个版本发布单元；进程、健康检查和资源边界清楚；保留当前 Dokploy Compose 生产/dev 项目；同一 MR preview 可端到端运行 | Compose 没有 Dokploy 原生 PR preview，需受限 workflow 管理栈与生命周期；Grove path 前缀和真实 Zio owner 必须补齐 | **选定**（用户 2026-10-08 选择）。 |
| 单进程/单容器 Dokploy Application | 可使用 Dokploy Application 原生 PR preview | 合并静态站点与 Grove 路由/生命周期；需在一个容器管理多个常驻服务或重写一个 server；增加应用耦合 | 不选。 |
| Vercel 只预览 Astro，Grove 复用 dev | 静态站点 MR preview 最少自动化 | Grove 不是同一 MR 的隔离运行实例；共享 dev 状态，不能证明完整集成 | 不满足本次“站点与 Grove 都包含”的要求。 |

## 8. 验收标准

1. 合并到 `dev` 更新 `zio-dev.lszio.space`，不改变 `zio.lszio.space`；合并/发布到 `main` 才更新生产。
2. 两个站点和 Grove 服务都来自同一 commit；`/grove/app/` 与 `/api/*` 在同一 origin 工作，现有 `/grove/` 介绍页保留并能进入控制面。
3. 首次启动可创建可写 store；真实 owner 执行队列任务并持久化进度及反馈消费事实；CPU worker 的 checkpoint 可验证并续训，不兼容 checkpoint 被拒绝且不覆盖已发布状态；服务重启后 run/events/checkpoint 可读。未配置 worker 或隔离时诚实报告不可用，不伪造训练完成。
4. Reader/annotator/operator/publisher 的跨角色拒绝行为在站点同源路径下保持；公开健康和静态资产不泄露私有 store 内容。
5. 合格 MR 的预览使用 MR head SHA、独立根/令牌、部署状态和 URL 可检查；新提交更新该预览；关闭/合并后服务、域名和专属数据 volume 均被清理。
6. 构建镜像中的服务不依赖源 checkout 或宿主 CWD；路径越权被拒绝，容器未使用 `privileged`，worker isolation 在实际运行目标上通过。
7. 可信 Zio core、执行器、评价器、授权和发布门槛在镜像及 worker 内保持不可写；可更新模块、权重和 checkpoints 与可信逻辑分离，正式发布仍要求人工批准。
8. Production Grove 数据根在首次写入/迁移前完成备份，并通过一次恢复演练；dev 与 MR 状态不共享该恢复目标。

## 9. 实施前必须确认的事实

- Dokploy 目标容器是否能在不授予 broad privilege 的情况下通过 Grove 的 user/mount/PID/network namespace jail 探测；未通过时不得启用训练执行。
- MR preview 区域的 wildcard DNS 与 TLS 是否可用；若不可用，预览使用 Dokploy 动态域名。
- 当前 canonical Zio API 中的 readonly store、缺失 owner loop、CWD 资产解析和占位 handler 必须按现有 Grove 计划修正并由真实服务验收。部署本身不证明这些语义已完成。
- 此设计没有授权生产部署或改变 Dokploy production 配置；生产切换仍需经过 dev 验收和明确发布步骤。
- `main`/`dev` 的 required-check branch protection 状态、Dokploy 可用资源与每 MR 配额尚未验证；实施前配置检查门槛、并发预览上限及 CPU/内存/磁盘上限。

## 10. 来源

### 仓库证据（2026-10-08）

- `docs/adrs.md:926-1006`：ADR-019 将 Grove 定位为独立应用并明确当前差距。
- `docs/superpowers/specs/2026-10-07-infrastructure-full-zio-design.md` 与 `docs/superpowers/plans/2026-10-05-zio-grove-convergence.md`：Grove 业务、宿主和迁移验收的既有设计/计划；本文只新增部署与环境合同。
- `docs/zio-architecture.md:37-47`：Grove 的 Zio 入口与当前仍在 Rust 中的业务边界。
- `apps/grove/main.zio:178-221`、`apps/grove/api.zio:376-400,534-553,629-669`、`apps/grove/store.zio:152-169`、`apps/grove/runner.zio:40-44,181-204`、`apps/grove/web/index.html:7,218-224`、`apps/grove/web/bridge.js:18-22,427`：当前 Zio service、runner 接缝和浏览器资源 URL。
- `tools/install.sh:30-109`、`langs/cli/src/main.rs:319-388`、`contribs/native/host/src/storage_fs.rs:132-155,238-240`：canonical launcher、授权根和相对读路径。
- `Dockerfile:1-24`、`docker-compose.dokploy.yml:1-8`、`nginx.conf:1-27`：当前静态站点构建和路由。
- `.github/workflows/ci.yml:3-7`：push/PR CI 触发器。
- Dokploy 当前只读查询：`zio` 为 `main` → `zio.lszio.space`；`zio-dev` 为 `dev` → `zio-dev.lszio.space`；两个 Compose 项目均 `autoDeploy=true`。

### 外部来源（2026-10-08）

- Dokploy Preview Deployments：<https://docs.dokploy.com/docs/core/applications/preview-deployments> — PR 预览为 Application 的 GitHub 集成能力，预览可按 PR 更新并在关闭/合并时清理；公共仓库需限制不可信构建。
- Dokploy Docker Compose：<https://docs.dokploy.com/docs/core/docker-compose> — Compose 可由 Git webhook 按选定分支部署。
- Dokploy GitHub integration：<https://docs.dokploy.com/docs/core/github> — 多个服务可跟踪同一仓库的不同分支，push 部署绑定所选分支。
