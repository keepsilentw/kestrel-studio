# kestrel-studio 文档索引

`kestrel-studio` 是一个 Web 生成式媒体 Agent：用户在网页里给一句话，LLM 流式思考、
调用生成工具、把产物渲染到页面上，可预览可下载。
技术形态为 NestJS 单进程 SSR + SSE 流式，无独立前端服务。

## 文档地图

| 文档 | 回答什么问题 | 什么时候读 |
|---|---|---|
| [architecture.md](./architecture.md) | 系统**是什么**：定位、技术选型、分层、关键设计决策、数据模型、Agent 循环、工具集、语音通路、目录与配置 | 动手改代码前 |
| [roadmap.md](roadmap.md) | 要**往哪走**：能力扩展（对话/图片/视频三模式）、多端形态、内核选型裁决与分期、当前待办 | 规划新功能前 |
| [implementation-notes.md](implementation-notes.md) | 搭建期撞到过哪些环境坑、根因是什么、怎么修的 | 重装环境，或遇到相似报错时 |
| [deployment.md](deployment.md) | 服务器上怎么构建、怎么暴露、怎么运维 | 部署与线上排障时 |
| [verification.md](verification.md) | 哪些行为经**实测**确认、具体数据是多少 | 引用端点或模型行为前（注意看日期） |

Claude 面向的部署操作手册（含完整排障流程）见 [.claude/skills/deploy/SKILL.md](../.claude/skills/deploy/SKILL.md)。

## 本地起服务

一键启动（构建前端资源与后端后交给 pm2 常驻后台）：

```bash
make install      # 首次
make start        # 构建并启动，打印访问地址与健康探测结果
make status       # 运行状态 + 健康探测
make logs         # 跟随日志
make stop         # 停止
```

访问 `http://localhost:8848`。建库时 seed 账号（密码经 bcrypt 哈希）：

| 账号 | 密码 | 角色 |
|---|---|---|
| `admin` | `123456` | 普通用户：只能看到自己的会话。**仅本地**——`NODE_ENV=production` 时不建号 |

超管账号不内置凭据，由环境变量注入（生产环境唯一的入口）：同时给出 `SUPER_ADMIN_USERNAME` 与
`SUPER_ADMIN_PASSWORD` 才会在首次启动时建出（多一个 `/admin`，可管账号、可只读查看
所有人的会话）；缺任一个则不建号，`/admin` 不可达。密码**只在首次建出时生效**，
改过之后重启不会重置（在 `/admin/users/:id` 改）。

权限模型见 [architecture.md](./architecture.md) §12。部署侧的密钥约定见 [deployment.md](./deployment.md) §5。

本地凭据可选：在项目根放一个 `.env`（已被 git 与 `.dockerignore` 排除），启动时会被
**叠加**进环境——只填没设过的变量，shell 里已导出的值优先。要让本地也建出超管，写
`SUPER_ADMIN_USERNAME` / `SUPER_ADMIN_PASSWORD` 两行即可；不给就是普通用户单机。

改代码时用 watch 模式，两个终端（pm2 占用同一端口，先 `make stop`）：

```bash
make dev          # 后端 watch（nest start --watch）
make web-watch    # 另开一个终端，前端资源 watch 构建
```

`make help` 列出全部命令；服务器侧运维命令见 [deployment.md](deployment.md) §6。

## 文档维护约定

本次拆分的依据是**关注点的生命周期**——不同内容的变化频率差了几个数量级，
混在一份文档里会互相干扰（改一句部署配置要动整份架构文档）。维护时按此约定归位：

- **每份文档只写自己那部分**，跨主题一律用链接，不复制内容。两份索引必然漂移，所以索引只有这一份。
- **带日期的实测结论**（端点行为、模型行为、性能数据）一律进 `verification.md`，
  它们是**证据**不是**设计**，会过时；引用前先看日期。
- **踩坑台账只增不减**（`implementation-notes.md`）。删掉一条，下一个人就会重踩一次。
- **`roadmap.md` 会被频繁改写**，`architecture.md` 尽量不动。
  当 roadmap 的结论落地后，把它从 roadmap 移进 architecture。
- 部署相关的坑放 `deployment.md`，实现期的坑放 `implementation-notes.md`；
  判据是「换台服务器还会有吗」。
