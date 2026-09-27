# kestrel-studio 部署与运维

> 本文只覆盖**服务器侧**：构建形态、端口、反代、域名与运维命令。
> 本地搭建期的环境坑见 [implementation-notes.md](implementation-notes.md)。
> Claude 面向的操作手册见 `.claude/skills/deploy/SKILL.md`（含排障流程与更多约束说明）。

## 1. 部署形态：服务器端零下载构建

服务器出站带宽只有约 **65 KB/s**（实测：下载 8.7MB 的 apt 索引 60 秒未完成），
而入站约 **5 MB/s**——相差 77 倍。因此任何需要服务器下载的方案都不可行，
包括 `apt install` 编译工具链、`pnpm install`、以及在本机建 VM 做交叉构建。

最终方案：镜像在服务器上构建，但**服务器不下载任何东西**。

| 需要的东西 | 来源 |
|---|---|
| gcc / g++ / make / python3 | `golang:1.24` 镜像（基于 buildpack-deps，自带工具链） |
| Node 运行时 | `node:24-bookworm-slim` 镜像，多阶段 `COPY --from` 复制 |
| node_modules | 开发机打包上传（5 MB/s） |

两个基础镜像都是 Debian 12 (bookworm)，glibc 一致，Node 二进制可直接跨镜像复制。
`node-gyp` 用 `npm_config_nodedir=/usr/local` 指向复制过来的头文件，避免下载 headers
（这是唯一还会触发网络的一步）。

上传的 node_modules 来自 macOS，其中的 better-sqlite3 是 darwin-arm64 的二进制，
因此在镜像构建时用服务器侧工具链重新编译一次。

### 相关约束

- **Node 不能直接装在宿主机上。** 服务器是 CentOS 7（glibc 2.17），
  Nest 12 / Vite 8 需要 Node 20+（glibc 2.28+）；实测官方 linux-x64 二进制报
  `GLIBC_2.27 not found`。这也是 pm2 方案不可行的原因。Docker 是唯一可行运行时。
- **基础镜像来自镜像站。** 服务器上 Docker Hub 不可达，用 `dockerproxy.net`
  （实测 `hub.rat.dev` 超时、`docker.nju.edu.cn` 直接失败、`docker.m.daocloud.io` 与
  `docker.1ms.run` 卡住）。首次需跑 `scripts/bootstrap-host.sh` 缓存两个基础镜像。
- **不要动 `/etc/docker/daemon.json`，不要重启 docker daemon。** 该机还跑着其他生产
  容器与 1Panel 的 openresty，重启会全部拉挂。镜像站按镜像逐个消费。

## 2. 端口不能放在 `.env` 里

现象：容器 `Up`、应用日志显示 `Nest application successfully started`，但宿主机 8848 探测不通。

根因：容器内 `PORT=3000`，应用监听 3000，而端口映射是 `8848:8848`。
服务器上的 `.env` 是第一次部署时创建的，那时写的还是 `PORT=3000`；而 `deploy.sh` 出于保护
`SESSION_SECRET` 的考虑**从不覆盖已存在的 `.env`**，于是那个过期值一直生效。

处理：端口属于部署配置，不属于密钥，改由 compose 注入：

```yaml
environment:
  - PORT=${HOST_PORT:-8848}
```

compose 的 `environment` 优先级高于 `env_file`，`.env` 里的残留值不再起作用。

教训：把一个"只写一次、永不更新"的文件当作配置源时，其中任何可变项都必须挪出去。

## 3. 对外暴露：openresty 反代

服务器的对外入口是 1Panel 管理的 openresty（容器名形如 `1Panel-openresty-*`，监听 80/443）。
该机上所有业务服务都不直接暴露端口，而是经它反代——`try.*` 系列域名都解析到同一台机器。
阿里云安全组也只放行了 80/443。

kestrel-studio 的站点配置：`/opt/1panel/www/conf.d/try.kestrel.justwork.link.conf`
（该目录映射进容器的 `conf.d`）。

三个关键点：

1. **上游写 `172.17.0.1:8848`，不能写 `127.0.0.1`**。openresty 自己也在容器里，
   `127.0.0.1` 指向它自身；`172.17.0.1` 是 docker0 网关，即宿主机。
2. **`proxy_buffering off` 必须开**。应用走 SSE 流式输出，nginx 默认会缓冲整个响应，
   流式会退化成「憋到最后一次性吐出」，功能特性直接失效。同时 `proxy_read_timeout`
   放宽到 300s——单次文生图期间连接会静默几十秒。
3. **语音 socket 需要单独一个 `location = /api/voice`**。第 2 点那段片段为了让 SSE 保持
   普通长连接，**显式把 `Connection` 清空成 `""`**，这会一并杀掉 WebSocket 的 upgrade；
   父级 server 块（1Panel 生成）本来设好了 `Upgrade`/`Connection "upgrade"`，被它覆盖掉了。
   新块用精确匹配（`=`）而不是前缀，这样 `POST /api/voice/ticket` 这种普通请求仍走上面的
   通用设置，不会莫名带上 `Upgrade`。`proxy_read_timeout` 放宽到 3600s——通话期间静默远长于
   文生图的几十秒（上游自己 180s 会关掉空闲会话）。

   已加在服务器上的 `proxy/kestrel.conf`（原文件备份为 `.bak`），`nginx -t` 通过并已 reload
   （openresty 日志有 `SIGHUP … reconfiguring`），随后经公网域名实测走通——见
   [verification.md](verification.md) §4.1。

改动流程：`docker exec <openresty 容器名> nginx -t` 验证语法，再
`nginx -s reload`（平滑重载，不断现有连接）。实测 reload 后现有 8 个站点均无回归。

## 4. 对外访问配置（2026-09-21 完成）

| 项 | 配置 |
|---|---|
| 域名 | `try.kestrel.justwork.link`（阿里云云解析 A 记录指向该机公网 IP） |
| 入口 | 1Panel 管理的 openresty，80 强制跳 443 |
| 站点配置 | `/opt/1panel/www/conf.d/try.kestrel.justwork.link.conf`（1Panel 生成） |
| 反代片段 | `/opt/1panel/www/sites/try.kestrel.justwork.link/proxy/kestrel.conf`（手工补） |
| 证书 | 由 1Panel 签发，随其证书更新任务（每日 00:00）自动续期 |

两处坑，都是 1Panel 的行为，不在代码里：

1. **建站时没有生成反代规则**。主配置里写着 `include .../proxy/*.conf`，但该目录并不存在；
   nginx 对通配符 include 的空匹配不报错，于是站点「创建成功」却没有任何反代，
   请求会落到 nginx 默认页。需手工创建该目录并补上片段。
2. **证书是合并签发的**。`try.*.justwork.link` 系列共用一张多域名证书，
   新建站点不会自动加入这张证书——要在 1Panel 里把新域名追加进去并**重新申请**。
   否则 1Panel 会把一张不含该域名的旧证书分配给新站点，表现为 TLS 握手报域名不匹配
   （`SSL_ERROR_BAD_CERT_DOMAIN`），而站点看起来一切正常。

## 5. 密钥管理

`.env` 只存在于服务器 `/opt/kestrel-studio/.env`，由 `scripts/deploy.sh` 在**首次部署时**创建：
从本地 cc-switch 库读取百炼 key，生成新的 `SESSION_SECRET`，并写入部署者当场导出的
`SUPER_ADMIN_USERNAME` / `SUPER_ADMIN_PASSWORD`——缺任一个脚本直接失败，不会建出一个
凭据可猜的超管。

**仓库与镜像里不含任何可用凭据**：超管口令不进代码、不入库，只活在那台机器的 `.env` 里。

脚本**永不覆盖已存在的 `.env`**——重新生成 `SESSION_SECRET` 会让所有已登录会话失效，
而服务器上没有 cc-switch 库可回退取 key。轮换 key 的做法是直接改服务器上的文件再
`make deploy-restart`。

## 6. 运维命令

| 命令 | 效果 |
|---|---|
| `make deploy` | 完整重新部署（`scripts/deploy.sh`） |
| `scripts/bootstrap-host.sh` | 每台新宿主机执行一次，缓存两个基础镜像 |
| `make deploy-status` | 查看容器状态 |
| `make deploy-logs` | 跟随容器日志 |
| `make deploy-restart` | 重启容器 |
| `make deploy-down` | 停止并移除容器（保留 `data/`、`storage/`） |

排障入口见 `.claude/skills/deploy/SKILL.md`；构建失败、容器启动即退出、容器内健康但外部不可达
三类问题的处置流程在那里。

## 7. 改名后的线上迁移（2026-09-22 已执行）

项目于 2026-09-22 从 `kestrel-image` 更名为 `kestrel-studio`——能力已从「文生图」扩展为
对话 / 图片 / 视频 / 语音，`-image` 名不副实。仓库与线上**都已完成改名**，本文档其余部分写的
`/opt/kestrel-studio` 与容器名就是服务器现状。

| 项 | 现状 |
|---|---|
| 远端目录 | `/opt/kestrel-studio` |
| 容器 / 镜像 | `kestrel-studio` / `kestrel-studio:latest` |
| 域名 | `try.kestrel.justwork.link`（不含 `-image`，**无需改**） |
| 反代上游 | `172.17.0.1:8848`（不依赖容器名与目录名，**无需改**） |

按上表预测的两项确实无需改动：域名与反代上游都不依赖容器名。迁移后实测：5 个生产容器
（4 个 Golang API + 1Panel openresty）全程未受影响，端口 8848 只有本项目占用。

### 迁移中唯一需要留意的两处

1. **旧库的 schema 会自动补齐，不需要手工迁移。** 迁移前的线上库还没有 `generation_tasks`
   表、`messages` 也没有 `mode` 列（那是任务子系统与轮次模式落地前建的库）。新版本首次启动时
   `applySchema` 的 `CREATE TABLE IF NOT EXISTS` 与 `ADDED_COLUMNS` 的 `ALTER TABLE` 会补上
   ——实测启动后两者都在，无需任何手工 DDL。这是那两个机制存在的意义，线上升级是它们的真实用例。
2. **迁移前用 `sqlite3` 读一次库会顺带把 WAL checkpoint 回主库**（最后一个连接关闭时 SQLite
   自动 checkpoint），所以 `-wal`/`-shm` 会消失、主库文件变大。这是正常且有利的结果，不是损坏。

### 旧目录仍留在盘上

`/opt/kestrel-image`（434 MB）**没有删除**，作为迁移前数据与密钥的现成回退副本。没有任何东西
引用它（compose 项目已随停栈移除，反代上游按 IP 不按名字），所以留着不影响运行。

确认新栈无问题后自行删除：

```bash
ssh lavo-test "ls /opt/kestrel-image"        # 先看一眼
ssh lavo-test "rm -rf /opt/kestrel-image"
```

（该目录由用户自己删，不由 Claude 代删。）

### 三个要点（下次迁移仍然适用）

1. **两个容器会争 8848 端口**，顺序不能颠倒：必须先停旧的再起新的。
2. **`.env` 必须复制而不是重新生成**。`deploy.sh` 只在目录下没有 `.env` 时创建它，
   重新生成会换掉 `SESSION_SECRET`（所有人被登出）与百炼 key。
3. **服务器上的资产路径不需要修**（现在连本地也不需要了）。`assets.file_path` 只存**文件名**，
   读取时按运行中的 `STORAGE_DIR` 解析（`src/media/asset-path.ts`）；旧行里的绝对路径仍然认，
   文件还在原处就原样用。所以容器里（cwd `/app`）与本地都一样工作，改目录名不再需要动数据。
   这条曾经是"本地必须手工修库"的坑，根因与改法见
   [implementation-notes.md](implementation-notes.md) §7。

## 8. 多账号上线（2026-09-22 已执行）

第二次 `make deploy`，把多账号 / 超管管理面 / 资产路径修复一起带上线。这次改动**碰了登录与
数据两处**，所以部署前后逐项核对了下面几条不变量：

| 关注点 | 结果 |
|---|---|
| `.env` 未被覆盖 | mtime 与 size 与部署前一致 —— `SESSION_SECRET` 不变，**已有登录会话不会被登出** |
| 数据卷未被替换 | `data/kestrel-studio.db` 的 mtime 与 size 与部署前一致；`storage/` 仍是 6 个文件 |
| 其他容器 | 同机其他业务容器与 1Panel openresty 全程 `Up`，未受影响；8848 仍只属于本项目 |
| 新代码已生效 | `/admin` 由部署前的 `404` 变为 `302 → /login`（路由在、守卫在） |
| 启动时自动迁移 | `users` 补 `role`、既有账号落为 `user`、建出超管账号 ——机制与真实库拷贝上的实测一致（[verification.md](verification.md) §5） |

**线上未验证的一项**：用超管账号登录后浏览管理页。这需要向线上提交凭据，被本机权限策略拦下，
没有绕过。要确认的话二选一：自己登录看一眼，或明确授权后由 Claude 跑一遍。
本地超管账号走通了全流程（建号 / 改密 / 只读回放 / 级联删除），机制本身不缺证据。
