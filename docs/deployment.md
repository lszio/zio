# 部署与环境契约

本文记录已落地的部署现状：一个 Dokploy Compose 栈、一个 `site` 服务
（`Dockerfile`：bun 构建 Astro 静态站点 → nginx 托管）。
Groiz 是独立项目（[lszio/groiz](https://github.com/lszio/groiz)），不在本仓库，
其运行与部署不在此栈内。

## 环境

| 环境 | Dokploy 栈 | 域名 | 分支 |
| --- | --- | --- | --- |
| 生产 | `zio` | zio.lszio.space | `main` |
| 开发 | `zio-dev` | zio-dev.lszio.space | `dev` |
| PR 预览 | 预览项目 | `zio-pr-<n>.<PREVIEW_ZONE>` | PR（`pull_request_target`，限同仓库 OWNER/MEMBER/COLLABORATOR 或 `workflow_dispatch`） |

两个常驻栈与 PR 预览都部署同一个 `docker-compose.dokploy.yml`。

## 什么跑在哪里

- `site`：nginx 托管 Astro 静态站点与 WASM 引擎，暴露 80。

栈内只 `expose`，不发布宿主端口——宿主端口是全局资源而 Dokploy 项目不是，
多个栈共享同一主机；本地端到端验证用 `docker-compose.local.yml` 覆盖把 80
映射到 `127.0.0.1:18080`：

```bash
docker compose -f docker-compose.dokploy.yml -f docker-compose.local.yml up
```

Dokploy 侧走内部网络按域名路由。

## 生产首次切换

1. 在 Dokploy 项目创建/更新栈 `zio`，指向仓库 `docker-compose.dokploy.yml`。
2. **手动改域名上游**：`zio.lszio.space` 的 domain upstream 从服务 `zio` 切到 `site`（compose 文件头部有注释提醒）。
3. 部署后验证：`curl -fsS -o /dev/null -w '%{http_code}\n' https://zio.lszio.space/` 返回 `200`；`curl -fsS https://zio.lszio.space/examples/manifest.tsv` 返回示例清单。

## 静态资源与 WASM

- `docs/`、`book/`、`blog/`、`examples/` 在镜像构建时复制到 nginx 根目录，
  直接以静态文件托管（`.md` / `.zio` / `.tsv` 以 `text/plain` 返回）。
- `apps/site/public/wasm/` 是提交的 WASM 产物。改过 `langs/core/src/` 或
  `langs/core/assets/std/core.zio` 后必须先跑 `./tools/build-wasm.sh` 再重建镜像，
  否则页面会静默运行旧引擎。
- nginx 缓存策略：`/_astro/` 哈希资产 `immutable`；`/wasm/` 为
  `max-age=0, must-revalidate`，确保引擎随构建更新。

## 备份与恢复

`site` 是无状态静态栈，没有需要备份的持久卷。回滚靠重新部署指定的
镜像/commit；Dokploy 栈本身的配置（域名上游、预览用 secrets/vars）按下列
运维清单维护。

## CI 与预览

- `.github/workflows/ci.yml`：push 覆盖 `main` 与 `dev`；jobs 为 `build and test`
  （含自举契约、fmt 与 clippy）、`wasm build`、`library contracts`、`language server`。
- `.github/workflows/preview.yml` + `tools/preview-stack.sh`：PR 预览，走 Dokploy API。已验证的端点：`compose.one/create/update/saveEnvironment/deploy/delete`、`domain.byComposeId/create/delete`（`deleteByComposeId` 不存在）。必需 secrets/vars：`DOKPLOY_PROJECT_ID`、`DOKPLOY_ENVIRONMENT_ID`、`DOKPLOY_GITHUB_ID`、`DOKPLOY_API_TOKEN`；vars：`DOKPLOY_API_URL`（裸主机名，不带 `/api` 后缀）、`PREVIEW_ZONE`。删除预览会同时删除域名与 compose。

## 手动运维清单（未自动化）

- [ ] GitHub 分支保护：`main`/`dev` 设必需检查——需在 GitHub 仓库设置手动配置。
- [ ] Dokploy 域名上游切换：`zio` 与 `zio-dev` 两个栈都从服务 `zio` 切到 `site`。
- [ ] 预览所需 `DOKPLOY_*` secrets/vars 与 `PREVIEW_ZONE` 在 GitHub 仓库配置。
