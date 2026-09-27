# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目定位

Web 生成式媒体 Agent：用户在网页给一句话，LLM 流式思考、调用生成工具、把图片或视频渲染到页面上。
能力按**轮次**分四种模式（`auto` / `chat` / `image` / `video`）；`auto` 由模型自行路由，显式模式收窄工具集。
此外还有两条并列入口：实时语音 `/voice`（见下）与超管的管理面 `/admin`。
账号分两级（`super` / `user`）：会话默认各自隔离，超管可**只读**查看普通账号的会话，见 `docs/architecture.md` §12。

`kestrel/`（Rust）是 Agent harness 内核；本项目是它面向生成式媒体的 Web 化实现，沿用「Agent 循环 + 工具注册表」的分层思路，语言换成 NestJS。

设计文档在 `docs/`，**动手前先按需读**：`architecture.md`（系统是什么，少变）、`roadmap.md`（将变成什么，常改）、`implementation-notes.md`（实现期踩坑台账，只增不减）、`deployment.md`（服务器侧）、`verification.md`（带日期的实测证据，引用前先看日期）。文档索引与维护约定见 `docs/README.md`。

## 命令

**Makefile 是命令入口**，`make help` 列全部；不要绕过它直接调 pnpm 脚本。

| 命令 | 说明 |
|---|---|
| `make install` | 安装依赖 |
| `make start` | 构建前端资源 + 后端，交给 pm2 常驻，打印访问地址与健康探测 |
| `make stop` / `restart` / `status` / `logs` | pm2 运维 |
| `make dev` | 后端 watch（`nest start --watch`） |
| `make web-watch` | 前端资源 watch 构建，开发时另开一个终端 |
| `make build` | `build-web` + `build-server` |
| `make typecheck` | 三份配置各跑一次 `tsc --noEmit`（后端 / 前端脚本 / 测试） |
| `make test` | 单元测试（`vitest run`），纯模块，无 DI |
| `make test-watch` | 单元测试 watch 模式 |
| `make clean` | 清 `dist/` 与 `public/`，保留 `data/` `storage/` |
| `make deploy*` | 部署到 lavo-test（见下） |

要点：

- 访问 `http://localhost:8848`。建库时 seed 账号（bcrypt 哈希入库，且**只在账号缺失时创建**，
  所以改过的密码不会被重启重置）：`admin` / `123456` 是**本地**引导账号（代码内置，
  `NODE_ENV=production` 时不建号）；超管（多一个 `/admin`）的凭据来自
  `SUPER_ADMIN_USERNAME` / `SUPER_ADMIN_PASSWORD`，**仓库里没有内置超管口令**，
  两个变量缺任一个就不建超管、`/admin` 不可达。这条约束存在的原因是仓库要公开：
  内置凭据等于给线上实例发可用登录。
- **pm2 与 watch 模式争 8848 端口**：跑 `make dev` 前先 `make stop`；`make dev` 会自己检测并警告。
- **质量门是 `make typecheck` + `make test`**。测试用 vitest，分两个 project（见 `vitest.config.mts`）：
  - `server`（node 环境）—— `src/**/*.test.ts`。走 **SWC** 而非默认的 esbuild，因为 esbuild 不实现 `emitDecoratorMetadata`，而应用有约 19 处构造参数依赖它；没有 SWC 就起不了 `Test.createTestingModule()`。这也是当初不用 ts-jest 的原因：ts-jest 要 TS 的 programmatic compiler API，而 TS 7 已移除（本项目钉在 TS 6.0.3 正是同一根因）。
  - `web`（jsdom 环境）—— `web/**/*.test.ts`，测 `web/scripts/main.ts` 的 DOM 行为。`web/scripts/test-setup.ts` 补了 jsdom 缺失的 `scrollIntoView`。
  - 测试里没有类型断言（全局 TS 规则禁止），所以守卫、控制器、登录链路都是**起真实 HTTP 服务再发请求**验证的，不是伪造 `ExecutionContext` 一类对象。
  - 覆盖到的：纯模块、`ToolRegistry`、`ConversationService` / `TaskService` / `TaskEventsService` / `TaskWorker`（真 SQLite 内存库 + 生产 DDL）、`AuthService`、`ensureAdminUser`、`chat` / `view` / `asset-frame` 控制器（含各端点的归属校验、签名帧的 403/404 边界、登录/登出会话链路）。供应商交互仍由注入边界 stub 掉。`local.strategy` / `session.serializer` / `login-redirect.filter` 没有独立测试文件，但被登录链路测试**行为性覆盖**（策略真被实例化并驱动、序列化由 cookie 往返证明、过滤器由"错误密码 → 302 /login?error=1"证明）。语音侧：服务端只测纯逻辑（票据存储、帧解析），中继按同样的边界归 `docs/verification.md` §4；**浏览器客户端测帧序列**（`web/scripts/voice.test.ts`，jsdom + 假 socket：按下取票、`onopen` 发 `start`、ready 前后音频排队与补发、commit 时机）。这一条是踩过坑才加的——只验"控制台干净"没抓住客户端漏发 `start` 帧、服务端静默等待的故障，见 `docs/implementation-notes.md` §8。账号面（`admin/`）测到守卫的两条分支与非超管的 403、`canRead` 的完整角色矩阵、账号 CRUD 与级联删除、管理页的真实渲染（含"密码框旁必须有可见账号名"这条浏览器契约）。软删除单独测了三件事：属主看不见、超管看得见、数据一条不少（消息/资产行/磁盘文件都断言了）。
  - **模板与客户端脚本的契约由一对测试守住**：`web/scripts/main.test.ts` 用手抄的 DOM fixture 测行为，`view/view.controller.test.ts` 渲染真模板并断言 `main.ts` 用 `requireElement` 取的那些 id 都在。改模板 id 会让后者失败。
  - **不做 UT 的范围是刻意划定的，不要再提议补**：agent 轮次循环（`agent/agent.service.ts`）与供应商客户端（`bailian/responses-client.ts`）——它们的行为由活的端点定义（非标准帧、reasoning 事件名、超时），伪造供应商只会把我们的假设重新编码一遍，且在供应商真的变了时照样通过。那部分证据带日期记在 `docs/verification.md`。同样不测的还有 `chat` / `view` 控制器与 `media` 的 HTTP、落盘路径。
- 改动前端后按全局规则用 Chrome DevTools MCP 验控制台。两个页面都已访查过：
  `/voice` **零条目**；对话页零错误零警告，另有 1 条 Chrome *issue*（`loading="lazy"` 的资产
  `<img>` 缺尺寸，属既有代码），记在 `docs/roadmap.md` §3。
  用 MCP 时**新开标签页**（`new_page`），别动用户自己的会话；用户已把待调试站点打开时除外。
- `.hbs` 模板不在 TS 编译产物里，由 `nest-cli.json` 的 `assets` 拷进 `dist/view/views`；watch 模式下自动同步，一次性构建则必须重新 `make build-server`。

## 架构

单个 Nest 进程，SSR + SSE，**没有独立前端服务**。三条边界是理解全局的关键：

### 1. 两条渲染路，互不重叠

hbs 只渲染首屏骨架（登录态、历史会话侧栏、页面外壳、模式选择器），`src/view/views/chat.hbs`；
**从第一个 token 起，主体区全部由客户端 JS 拼 DOM**，服务端不吐 HTML 片段。
守住这条边界，不要在 hbs 里塞流式占位逻辑。

前端是无框架的原生 TS + Vite：`web/scripts/main.ts`（约 1000 行，发送、SSE 解析、增量渲染、会话流订阅、下载弹窗）＋ `web/styles/main.css`。
Vite 只做打包，产物固定为 `public/assets/main.js` / `main.css`（由 `vite.config.mts` 指定），模板以 `/assets/main.js` 模块化引入；`body[data-conversation-id]`、`[data-mode]`、`[data-task-id]` 等 data 属性是两条路的交接面。
**管理页（`/admin` 系）刻意不引任何客户端脚本**：纯服务端表单 + 302 + `?notice=` 提示，因此它们的模板不进 `main.ts` 的 id 契约，也没有控制台噪音。
`web/` 有自己的 `tsconfig.json`（DOM lib、`noEmit`），所以类型检查要跑两次。

### 2. 两条 SSE 流，用途不同

| 流 | 端点 | 生命周期 |
|---|---|---|
| 轮次流 | `POST /api/chat` | 随该 POST 存亡；body 带 prompt 与 mode |
| 会话流 | `GET /api/conversations/:id/events` | 长连接，推送**没有请求在飞**时发生的事件（视频渲染完成） |

服务端两条流都用 `src/common/sse.ts` 的 `SseWriter` **裸写 `@Res()`**，不用 `@Sse()` 装饰器——后者只支持 GET，而发消息需要 POST body。
每 15s 发 `: ping` 注释帧保活（文生图期间连接静默几十秒，中间层会判定空闲掐断），并带 `X-Accel-Buffering: no`（加压缩中间件会破坏 SSE）。

**前端两条流的消费方式不同，别搞混**：

| 流 | 客户端 |
|---|---|
| 轮次流 | 手写 `fetch` + `ReadableStream` 切帧（`readSseFrames`）——`EventSource` 不能 POST body，手写还能 abort |
| 会话流 | 真 `EventSource`，初始化时打开，`connected` 报出新 id 时重开 |

轮次事件：`connected` `reasoning` `text` `tool_call` `tool_result` `asset` `done` `error`；
会话事件：`task_updated` `message_added`。事件名的真源是 `src/common/sse.ts` 里的闭合联合类型，`docs/architecture.md` §6 只有轮次那 8 个（会话事件是后加的，那份表的「SSE 事件表」尚未补全）。

### 3. 同步轮次 vs 异步任务

- **图片是同步的**：`generate_image` 在轮次内完成（单次工具超时 120s），产物以 `asset` 事件推给前端。
- **视频是异步的**：`generate_video` 提交后**立刻返回** task id 并结束该轮，落 `generation_tasks` 表；
  `src/task/task.worker.ts` 后台退避轮询（8s 间隔，任务上限 20min），成功后**立刻**把字节镜像到 `storage/`，
  再往会话追加一条新的 assistant 消息并推 `task_updated` / `message_added`。

  两条硬约束决定了这个形状：视频渲染要几分钟（塞不进 SSE 轮次），且**供应商返回的媒体 URL 是短时效的**——错过成功那一刻就再也取不回。因此 `assets.source_url` 只作审计，**不作持久引用**。

  工具描述里明确要求模型「提交后不要轮询」，`get_video_task` 只在用户主动问进度时才该被调用。

### 模块职责

`AppModule`（`src/app.module.ts`）装配：`config` 读环境变量 → `database` drizzle + 建表 + seed → `auth` / `conversation` / `media` / `task` / `agent` / `chat` / `view`。

- `agent/agent.service.ts` — 轮次引擎：先生成一条**空的 assistant 占位行**（本轮产物要挂 `messageId`），组装 instructions 与工具声明，最多 `MAX_TOOL_ROUNDS = 5` 次 LLM 往返防死循环，把增量推给 `SseWriter`。
  `finally` 里必定收尾（补全占位行、`touch` 会话、关流）——**这个保证是承重的**：占位行若被留下，用户消息就成了孤儿，下一轮回放会出现两条连续 user 消息，曾因此**重复提交过一次付费视频任务**。
- `agent/mode.ts` — 模式的**唯一真源**：`TOOL_NAMES_BY_MODE` 决定每轮暴露哪些工具，`MODE_INSTRUCTIONS` 决定提示词片段。前端 `web/scripts/main.ts` 里有一份 `MODE_LABEL` 的重复定义，改中文标签要两处一起改。模式是**用户在 UI 上每轮选的**（`localStorage` 的 `kestrel-studio:mode`），不是模型自己挑的。
- `agent/tools.ts` — `ToolRegistry`：三个工具按名注册，`specsFor(mode)` 按模式裁剪。加工具 = 写 `AgentTool` + 进构造器数组 + 在 `mode.ts` 的 `TOOL_NAMES_BY_MODE` 里挂到对应模式。
  注意 `tool_choice` 在 `responses-client.ts` 里**写死 `'auto'`**：模式只收窄可见工具集，不强制调用——`image` 模式下模型仍可能只回文本。
- `bailian/responses-client.ts` — 手写 fetch + SSE 解析的 Responses API 流式客户端。**不引入 `openai` SDK**：端点每帧附 `:HTTP_STATUS/200` 与 `id:N` 非标准注释行，事件模型已完全摸清，SDK 只增加不确定性（依据见 `docs/verification.md` §1）。
- `bailian/token.ts` — 凭据解析优先级：`BAILIAN_API_KEY` / `DASHSCOPE_API_KEY` → cc-switch 库 `providers` 表 → `~/.codex/config.toml` 的 provider block。
- `media/media.service.ts` — 百炼文生图 / 提交视频任务 / 查询视频任务 / 落盘。
  **同一个 base URL 下有四条上游路径，别混**：chat 走 `/compatible-mode/v1/responses`（Responses API），出图走 `/api/v1/services/aigc/multimodal-generation/generation`，提交视频走 `/api/v1/services/aigc/video-generation/video-synthesis`，轮询走 `/api/v1/tasks/{id}`。
- `conversation/conversation.service.ts` — 会话、消息、资产的读写与**可见性规则**（`canRead` 读 / `isOwnedBy` 写，共用私有的 `readableBy()`），也是给 agent / tools / view / admin 共用的查询层。两个删除并存：`softDeleteConversations()`（用户侧，只打标记）与 `deleteConversations()`（账号删除的硬级联，含删文件）。
- `chat/` — HTTP 面：`chat.controller.ts`（轮次流、会话列表、消息回放、任务查询、下载、**软删除会话**）、`asset-frame.controller.ts`（见下）。
  删除是 `POST /api/conversations/:id/delete`，**只写 `deleted_at`**：属主看不见了，消息/资产/文件全留着，超管照旧能看（`docs/architecture.md` §12.1）。因此它只对普通账号开放，对超管返 403。
  **读端点走 `ConversationService.canRead`，写端点走 `isOwnedBy`**——超管能看别人的会话但发不进去。
- `admin/` — 超管管理面：`admin.guard.ts`（未登录 → `/login`；非超管 GET → `/`、POST → 403）、`admin.service.ts`（账号 CRUD + 级联删库外文件）、`admin.controller.ts`（纯表单 + 302 + `?notice=`）。它自己不写任何可见性查询，只用 `ConversationService`。
- `view/view.controller.ts` — hbs 页面：`GET /login`、`GET /`（`?c=` 指定会话）、`GET /voice`；共用的模板预处理（markdown、轮次锚点、时间格式化）在 `view/render.ts`，管理页也用它。

### 语音：第二条传输通路（`src/voice/` + `web/scripts/voice.ts`）

`/voice` 是一条**独立于轮次/SSE 的通路**，不是第五个模式（设计与理由见 `docs/architecture.md` §11）。链路：

```
浏览器 ⇄(wss /api/voice)⇄ Nest 中继 ⇄(wss …/api-ws/v1/realtime)⇄ 百炼 realtime-plus
```

- **服务端代理是必须的**：凭据不能进浏览器。服务端只转发与编排，不做音频编解码。
- **线上还需要一个单独的 nginx location**（`location = /api/voice`），因为现有那个片段为了让 SSE
  保持普通长连接而清空了 `Connection`，会一并杀掉 WS 的 upgrade。细节与理由见 `docs/deployment.md` §3。
- **鉴权走一次性票据**，不在 upgrade 里碰 session：`POST /api/voice/ticket`（挂 `AuthenticatedGuard`）
  签发单次使用、60s 有效的票据；WS 用它握手。理由与同形先例见 `src/common/signed-url.ts`
  ——守卫够不着调用方时改用短时效令牌。
- **推挽模式下 `commit` 之后必须显式 `response.create`**，服务端不会自动应答；`turn_detection`
  推挽要写 `null`（`'none'` 会被拒）。两条都在 `docs/verification.md` §4 有实测记录。
- **音频是 24kHz/16bit/单声道裸 PCM**，两端一致，所以服务端不做任何音频处理。
- **上游对未提交音频有 30 秒上限**：超时后它对后续每一帧 `append` 都回
  `Input audio buffer exceeded maximum duration (30s)`（客户端 43ms 分帧 ≈ 每秒 23 条），所以
  客户端把单段按住截在 25 秒（`MAX_HOLD_MS`，到点自动提交并提示用户），`blocked` 之后本次按住
  不再送音频；中继在上游拒绝后也不再灌缓冲区，且一次失败只往页面发一行。实测见
  `docs/verification.md` §3.2、§7.1。
- 语音轮次**不写 `mode`**（`null`），也不进入 `TOOL_NAMES_BY_MODE` 的裁剪——工具集由
  `session.update` 单独声明（`ToolRegistry.allSpecs()`）。语音也不回放历史给模型：上游会话
  自带上下文，落库只为历史与 UI。
- 工具分发与文本路径**共用** `ToolRegistry.execute()` / `describeOutcome()` / `assetUrl()`，
  不要在这条路径上另抄一份。

### 资产的两种取法

| 端点 | 鉴权 | 用途 |
|---|---|---|
| `GET /api/assets/:id/download` | session 守卫 | 页面内下载 |
| `GET /api/assets/:id/frame` | HMAC 签名（`?exp=&sig=`），**故意不挂守卫** | 图生视频时交给供应商自己去取首帧 |

签名逻辑在 `src/common/signed-url.ts`：以 `SESSION_SECRET` 对 `assetId.expiresAt` 签 HMAC，TTL 2h，`timingSafeEqual` 比对。
供应商要能访问到它，所以图生视频强依赖 `PUBLIC_BASE_URL` 指向公网地址；`isPubliclyReachable()` 会在提交前拦掉 localhost 并给出真实原因，否则要等几分钟后拿一个供应商侧的下载错误。

## 编码约定

- **DI 只有两个 token**：`SQLITE_CONNECTION`（原始 better-sqlite3）与 `DRIZZLE_INSTANCE`（drizzle），都从 `database.module.ts` 导出。**没有 repository 层**——service 直接注入 `DRIZZLE_INSTANCE`，service 自己就是数据层。`DatabaseModule` 是 `@Global()`，功能模块不要 import 它。
- **`loadConfig()` 是普通函数，不是 DI**。`@nestjs/config` 虽然写在 `dependencies` 里，但全仓库没有任何 `ConfigModule` / `ConfigService` 用法；别顺手引入。
- **两个守卫是刻意的**（`auth/guards.ts`）：`AuthenticatedGuard` 给 JSON 端点返 401，`ViewAuthGuard` 给 HTML 路由 302 到 `/login`。即便挂了守卫，controller 里仍会再查一次 `req.user === undefined`。
- **文案按受众分语言**：给用户与供应商的字符串用中文（`'未登录'`、任务状态标签），**回灌给模型的字符串用英文**（`'prompt is required.'`、工具输出）。HTTP 层只有守卫抛 Nest 异常，controller 一律手写 `res.status(...).json(...)`。
- **模型给的参数一律不信**：走 `src/common/args.ts` 的 `readString` / `readNumber` / `readOptionalNumber` / `clamp`，路由与 query 走 `common/http.ts` 的 `readPositiveInt`。
- **归属校验是手写的、没有拦截器**：每个碰会话的端点都自己调 `isOwnedBy` / `findAssetForUser`。新增端点必须照做。

## 数据与 schema

**两处要同时改**：`src/database/schema.ts`（drizzle 类型）与 `src/database/database.module.ts` 里的手写 `DDL`。
`CREATE TABLE IF NOT EXISTS` 加不了列，而 `data/` 是 docker volume 会跨部署存活，所以**首次发布后新增的列**还要进 `ADDED_COLUMNS` 数组（`ensureColumns` 会 `ALTER TABLE` 补上）。`messages.mode` 目前**只在 `ADDED_COLUMNS` 里**、不在 DDL 里，于是新库与老库经不同路径到达同一形态——加新列时别只补一处。正例是 `users.role` 与 `conversations.deleted_at`：两处都写了，老库分别由 `ALTER TABLE … DEFAULT 'user'` 与 `ADD COLUMN … INTEGER`（可空）补上，既有数据自动落到正确形态。

- `sessions` 表由 `better-sqlite3-session-store` 独占管理，**故意不建模进 drizzle**，避免两个写入方共管一张表。
- 需要数据迁移的 schema 变更走 `pnpm db:push`（drizzle-kit，schema 路径写死在 `drizzle.config.ts`）。
- 建表逻辑由 `database.module.ts` 的 `applySchema(connection)` 统一施加，测试复用它而不是复制 DDL。**测试里请自己 `new Database(':memory:')`**：`openDatabase()` 会把值过一遍 `resolve()`，所以 `DATABASE_FILE=:memory:` 会变成一个真的叫 `:memory:` 的文件，而不是内存库。
- `reasoning` 与 `tool_calls`（JSON 文本）单独存列，目的是刷新或重进会话能**完整回放思考过程**，而不只剩最终文本。
- 图片/视频二进制不入库，只存元数据 + 磁盘路径（`storage/`）。落盘文件名是随机串、扩展名按 kind 白名单兜底（不信任 URL），provider 的 URL 从不外泄（会过期）。
- **`assets.file_path` 存的是文件名，不是路径**，读取时由 `src/media/asset-path.ts` 的 `resolveAssetPath()` 按当前 `STORAGE_DIR` 展开（旧行的绝对路径若还在原处仍兼容）。写库时用 `storedAssetName()`。**不要把绝对路径塞进数据**：项目搬一次目录，历史资产就会对属主也 404，见 `docs/implementation-notes.md` §7。
- 正文字段的 markdown 是**服务端用 `marked` 渲染**成 HTML 交给模板三花括号输出的，客户端在每个 `text` delta 上增量重渲染；`reasoning` 按纯文本追加，不解析。

## 配置

`src/config/configuration.ts` 是唯一的读取点，`loadConfig()` 到处被直接调用（不是 Nest 的 ConfigService 注入）。

入口 `src/main.ts` 在**第一次读环境之前**调 `loadEnvFile()`（`src/config/env-file.ts`），把项目根的
`./.env` 叠进 `process.env`。三条规则要记住：**已存在的变量一律不覆盖**（容器里由 docker `env_file`
注入，镜像内也没有 `.env`，所以真实环境永远赢；`PORT=3000 make dev` 也不会被本地文件顶掉）；
文件不存在不是错误；值按字面取（第一个 `=` 之后全部保留，含 `#`，只剥掉成对包裹的引号）。
没用 `@nestjs/config` 的 `ConfigModule`，因为它在模块初始化期才写环境，而 `main.ts` 与各
provider 工厂在此之前就已经直接调 `loadConfig()` 了。`drizzle.config.ts` 不走这条路，
它直接读 `process.env.DATABASE_FILE`。

| 变量 | 默认 | 说明 |
|---|---|---|
| `PORT` | 8848 | |
| `NODE_ENV` | 未设置 | `production`（Dockerfile 设）时不 seed 本地引导账号 `admin` |
| `SESSION_SECRET` | 非 production 下每进程随机；`NODE_ENV=production` 时缺失直接启动失败 | 同时是资产签名密钥（`src/common/signed-url.ts:19`） |
| `DATABASE_FILE` | `data/kestrel-studio.db` | |
| `STORAGE_DIR` | `storage` | |
| `PUBLIC_BASE_URL` | `https://try.kestrel.justwork.link` | 图生视频的帧 URL 前缀 |
| `SUPER_ADMIN_USERNAME` / `SUPER_ADMIN_PASSWORD` | 无（不建超管） | 超管账号凭据，只在首次建号时生效；空白即视为未配置 |
| `BAILIAN_BASE_URL` | Token Plan 端点 | |
| `BAILIAN_CHAT_MODEL` | `deepseek-v4.1-flash` | |
| `BAILIAN_REASONING_EFFORT` | `high` | 取值 `minimal\|low\|medium\|high`，见下方「容易踩的坑」 |
| `BAILIAN_VIDEO_MODEL_T2V` / `_I2V` | `happyhorse-1.1-t2v` / `-i2v` | |
| `BAILIAN_VOICE_MODEL` | `qwen-audio-3.0-realtime-plus` | `/voice` 的实时语音模型；`/api-ws/v1/realtime` 的地址由 `BAILIAN_BASE_URL` 换协议头得来 |

## 容易踩的坑

- **任何新 tsconfig 都要显式写 `types`。** TS 6 起 `types` 未指定**不再**自动包含 `node_modules/@types`（编译器里是 `usesWildcardTypes` = `some(options.types, t => t === "*")`，未指定即空）。本项目一直能用 Node 全局量，靠的是模块图顺带解析：`src/main.ts` import `express` → `@types/express` → `@types/node`。所以一个只以测试文件为根的 program 不 import express，`process` / `Buffer` / `node:*` 全部变成未解析名。`tsconfig.test.json` 里的 `"types": ["node"]` 是必需的，不是风格选择。
- **`src/alias-bootstrap.ts` 必须是入口的第一个 import**。TS 不为产物重写 `@/*` 别名，运行时靠它注册解析钩子；任何在它之前被 require 的模块都会漏掉别名。因此它显式传 `baseUrl`（TS 7 移除了 `baseUrl`，tsconfig-paths 读 tsconfig 会抛 `ERR_INVALID_ARG_TYPE`），`start:prod` 也就不需要 `-r tsconfig-paths/register`。
- **登录必须显式 `req.logIn()`**。`@nestjs/passport` 的 `AuthGuard.canActivate` 只把结果挂到 `request.user`，**从不写 session**；漏了这一步会表现为「登录 302 成功但没有 Set-Cookie，后续全部 401」。见 `auth/auth.controller.ts`。
- **`PORT` 不放 `.env`，由 compose 注入**。`.env` 是「只写一次、永不覆盖」（保护 `SESSION_SECRET`），里面任何可变项都会变成永久生效的过期值；compose 的 `environment` 优先级高于 `env_file`。
- **`.dockerignore` 的每条路径都要带前导斜杠锚定**；`deploy.sh` 的打包用**显式白名单**而非 `tar --exclude`。未锚定的 `storage` 会在任意深度匹配，静默删掉 `node_modules/.pnpm/multer@*/node_modules/multer/storage`，运行时表现为 `Cannot find module './storage/disk'`，而 NestJS 会把它转述成误导性的「`@nestjs/platform-express` 没装」。踩坑全过程见 `docs/implementation-notes.md` §6。
- **`make restart` 起不来时先看 pm2 条目的 `exec path`。** 项目从 `playground/kestrel-studio/` 搬到
  现在的位置后，pm2 里那条老条目仍指向旧目录（`--update-env` 的 restart 不会改 script 与 cwd），
  于是 `online` 但没有 pid、健康探测无响应。修法是 `make stop` 再 `make start`（后者用 `$(CURDIR)`
  重建条目）。`make start` 带健康探测，是判断这一步是否成功的依据。
- **不要动服务器的 `/etc/docker/daemon.json`，不要重启 docker daemon**——那台机器上还跑着四个生产容器与 1Panel 的 openresty。
- **带密码框的表单必须放一个可见的账号名输入框，`type="hidden"` 不算数。** Chromium 会为此在
  控制台报 DOM 提示（"Password forms should have (optionally hidden) username fields"），提示语里的
  "optionally hidden" 在实测里不成立；`readonly` 的可见字段才满足。对照实验见
  `docs/verification.md` §5.2。写管理页那类表单时照 `views/admin-user.hbs` 抄。
- **`reasoning` 事件依赖模型行为**：当前模型在 Responses API 上以 `response.reasoning_text.delta` 吐思考内容（正文走 `response.output_text.delta`）。换模型要重新确认；若不吐，按 `docs/architecture.md` §4.4 的降级方案把思考区退化为工具调用时间线，前端事件模型不变。
- **`reasoningEffort` 默认 `high` 是有实测依据的**：供应商默认档下出图请求只产生约 3 个 reasoning delta（模型直奔工具调用），`high` 档同请求 >1300 个；思考面板只在高档位才值得展示。

## 部署

目标是 `lavo-test`（CentOS 7 + Docker），域名 `try.kestrel.justwork.link`，远端目录 `/opt/kestrel-studio`。
完整操作手册与排障流程见 `.claude/skills/deploy/SKILL.md`，服务器侧细节见 `docs/deployment.md`。三条最容易误改的约束：

- 服务器出站带宽约 65 KB/s、入站约 5 MB/s，**相差 77 倍**。因此镜像在服务器上构建但**不下载任何东西**：工具链来自已缓存的 `golang:1.24`，Node 运行时从 `node:24-bookworm-slim` 多阶段 `COPY --from`，`node_modules` 从开发机打包上传。**不要「简化」回单阶段拉公共基础镜像**。
- Node 装不了在宿主机上（glibc 2.17 vs 需要 2.28+），所以 pm2 方案不可行，Docker 是唯一运行时。
- `.env` 只存在于服务器，`deploy.sh` **永不覆盖已存在的**（重生成 `SESSION_SECRET` 会把所有人登出，且服务器上没有 cc-switch 库可回退取 key）；轮换 key 直接改服务器文件再 `make deploy-restart`。

## 文档维护

改代码后按关注点归位，不复制内容：

- 设计变了 → `architecture.md`（尽量少动）；规划变了 → `roadmap.md`（频繁改写，结论落地后移进 architecture）。
- 新的实测结论（端点 / 模型行为、性能数据）→ `verification.md`，**必须带日期**，它是证据不是设计。
- 新踩的环境坑 → 按「换台服务器还会有吗」分流：会有 → `deployment.md`，不会 → `implementation-notes.md`（只增不减）。
- 项目路径在文档里出现 `playground/kestrel-studio/` 是历史遗留，实际位置以仓库为准。
