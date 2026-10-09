# 部署与环境契约

本文记录已落地的部署现状：一个 Dokploy Compose 栈、两个服务（`site` + `grove`）。
设计文档见 [specs/2026-10-08-unified-zio-grove-runtime-design.md](superpowers/specs/2026-10-08-unified-zio-grove-runtime-design.md)。

## 环境

| 环境 | Dokploy 栈 | 域名 | 分支 |
| --- | --- | --- | --- |
| 生产 | `zio` | zio.lszio.space | `main` |
| 开发 | `zio-dev` | zio-dev.lszio.space | `dev` |
| PR 预览 | 预览项目 | `zio-pr-<n>.<PREVIEW_ZONE>` | PR（`pull_request_target`，限同仓库 OWNER/MEMBER/COLLABORATOR 或 `workflow_dispatch`） |

两个常驻栈与 PR 预览都部署同一个 `docker-compose.dokploy.yml`。

## 什么跑在哪里

- `site`：nginx 托管 Astro 静态站点与 WASM 引擎，暴露 80。
- `grove`：Zio Grove 应用（`Dockerfile.grove`），HTTP 绑定 `0.0.0.0:8787`，CPU 训练 worker，数据在命名卷 `grove-data`（挂载于 `/srv/grove/data`）。
- 健康检查定义在镜像内（`Dockerfile.grove` 的 `HEALTHCHECK`），要求响应体 `status == "ok"`；调度器降级时报告 unhealthy。

`8787` 与 `80` 仅映射到宿主 `127.0.0.1` 供本地端到端验证；Dokploy 走内部网络路由域名，不对外发布端口。

## 令牌

`grove` 通过环境变量读取 `GROVE_TOKEN_READER` / `GROVE_TOKEN_OPERATOR` / `GROVE_TOKEN_PUBLISHER`，HTTP `Authorization: Bearer <token>` 按变量映射到角色。令牌未设置时服务以空令牌启动；非 loopback 地址在无任何 `GROVE_TOKEN_*` 时拒绝绑定（见 `apps/grove/api.zio`）。

首次部署前必须在 Dokploy 栈环境里生成并设置这些令牌，例如 `openssl rand -hex 32`，令牌属于机密，不进仓库。

## 生产首次切换

1. 在 Dokploy 项目创建/更新栈 `zio`，指向仓库 `docker-compose.dokploy.yml`。
2. 在栈环境设置三个 `GROVE_TOKEN_*`。
3. **手动改域名上游**：`zio.lszio.space` 的 domain upstream 从服务 `zio` 切到 `site`（compose 文件头部有注释提醒）。
4. 部署后验证：`curl -fsS https://zio.lszio.space/` 返回站点；`curl -fsS http://grove:8787/healthz`（栈内）返回 `status == "ok"`。

## 训练与 seccomp

用户命名空间训练需要 `unshare(CLONE_NEWUSER)`；Docker 默认 seccomp 使其返回 EPERM。`grove` 服务设 `security_opt: seccomp:unconfined`（实测：加此配置探针通过，去掉则 EPERM；未授予任何 capability）。镜像不具备命名空间时，训练任务诚实落到 `failed` 并报隔离错误，绝不伪造完成；具备时排队任务落到 `evaluating`。

## 备份与恢复

数据卷名：Dokploy 栈下为 `grove-data`，本地 `docker compose -p zio` 运行为 `zio_grove-data`。

```bash
# 备份（短暂停 grove，保证一致）
docker compose -p zio -f docker-compose.dokploy.yml stop grove
docker run --rm -v zio_grove-data:/data -v "$PWD":/backup alpine \
  tar czf /backup/grove-data-$(date -u +%Y%m%dT%H%M%SZ).tar.gz -C /data .
docker compose -p zio -f docker-compose.dokploy.yml start grove

# 恢复演练：先恢复进一次性卷，确认内容后删除
docker volume create grove-restore-drill
docker run --rm -v grove-restore-drill:/data -v "$PWD":/backup alpine \
  tar xzf /backup/grove-data-<timestamp>.tar.gz -C /data
docker volume rm -f grove-restore-drill
```

## CI 与预览

- `.github/workflows/ci.yml`：push 覆盖 `main` 与 `dev`；`grove-service` job 跑 HTTP harness、浏览器契约与 `tools/test-grove-deployment.sh`。
- `.github/workflows/preview.yml` + `tools/preview-stack.sh`：PR 预览，走 Dokploy API。已验证的端点：`compose.one/create/update/saveEnvironment/deploy/delete`、`domain.byComposeId/create/delete`（`deleteByComposeId` 不存在）。必需 secrets/vars：`DOKPLOY_PROJECT_ID`、`DOKPLOY_ENVIRONMENT_ID`、`DOKPLOY_GITHUB_ID`、`DOKPLOY_API_TOKEN`；vars：`DOKPLOY_API_URL`（裸主机名，不带 `/api` 后缀）、`PREVIEW_ZONE`。删除预览会同时删除域名、compose 与卷。

## 手动运维清单（未自动化）

- [ ] GitHub 分支保护：`main`/`dev` 设必需检查（含 `grove-service`）——需在 GitHub 仓库设置手动配置。
- [ ] Dokploy 域名上游切换：`zio` 与 `zio-dev` 两个栈都从服务 `zio` 切到 `site`。
- [ ] 三个 `GROVE_TOKEN_*` 在两个常驻栈的 Dokploy 环境里生成并设置。
- [ ] 预览所需 `DOKPLOY_*` secrets/vars 与 `PREVIEW_ZONE` 在 GitHub 仓库配置。
