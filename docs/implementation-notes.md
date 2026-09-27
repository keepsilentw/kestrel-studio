# kestrel-studio 实现记录（环境踩坑）

> 只增不减的台账：搭建过程中实际撞到并解决的问题，留档以免重装时重复排查。
> 部署环节的坑（构建、端口、反代）见 [deployment.md](deployment.md)。

## 1. 依赖安装

| 现象 | 根因 | 处理 |
|---|---|---|
| `better-sqlite3` 构建失败，`sh: node-gyp: command not found` | pnpm 环境不提供 node-gyp，该包又没有命中预编译二进制，退化成 `node-gyp rebuild` | 把 `node-gyp` 装进本项目 devDependencies，`node_modules/.bin/node-gyp` 即可见 |
| `ERR_PNPM_IGNORED_BUILDS` | pnpm 12 默认不执行依赖的 lifecycle 脚本 | 在 `pnpm-workspace.yaml` 写 pnpm 12 的新格式 `allowBuilds: { better-sqlite3: true, esbuild: true }` |

## 2. TypeScript 7 移除了 baseUrl

TS 7 报 `TS5102: Option 'baseUrl' has been removed`，paths 必须写成相对路径 `./src/*`。

连带影响运行时：`tsconfig-paths` 的 `mapping-entry` 用 `path.resolve(baseUrl, ...)` 拼绝对路径，
baseUrl 缺席时拿到 `undefined`，直接抛 `ERR_INVALID_ARG_TYPE` —— 别名解析整个失效。

处理：`src/alias-bootstrap.ts` 显式传 `baseUrl` 与 `paths` 注册解析钩子，并作为入口的
**第一个 import**（若别名在它之前被 require 就会漏掉）。因此 `start:prod` 不再需要
`-r tsconfig-paths/register`。

## 3. 其它

- `drizzle-orm` 0.45 没有 `node-sqlite` 驱动，所以不能改用 Node 内置 `node:sqlite`
  （那样会失去 ORM）。better-sqlite3 必须编译出来。
- `hbs` 无内置类型，需 `@types/hbs`。
- `better-sqlite3-session-store` 无类型定义，在 `src/types/` 下补齐声明。
- `bcryptjs` 自带类型，不要装 `@types/bcryptjs`（那是 stub 包）。
- `better-sqlite3-session-store` 是 GPL-3.0-only，个人项目无碍，对外分发需注意。

## 4. @nestjs/passport 的 AuthGuard 不会写 session

现象：登录返回 302 到 `/`，但响应**没有 Set-Cookie**，`sessions` 表 0 行，后续所有请求仍 401。

根因：`@nestjs/passport` 的 `AuthGuard.canActivate` 只调用 `passport.authenticate`，把结果挂到
`request.user` 就返回，**从不调用 `req.logIn`**。少了这一步，session 永远不会被写入——认证通过
和登录成功是两件事。

处理：`AuthController.login` 里显式 `req.logIn(user, cb)` 之后再重定向。

## 5. TypeScript 6 的构建约束

Nest CLI 12 需要 TypeScript 的 programmatic compiler API，而 TS 7.0 只提供 `tsc` 可执行文件
（该 API 预计 7.1 回归），因此依赖降到 TS 6.0.3。TS 6 另有三处要求：

- `include` 只能放 `src/**/*`。否则 TS 推断的 common source directory 是项目根，产物落到
  `dist/src/`，`main.ts` 里的 `__dirname` 相对路径与 hbs 模板位置全部错位。
- 必须显式声明 `rootDir: "./src"`（否则 TS5011）。
- 应用不需要 `.d.ts`，关闭 `declaration`；开着会让 drizzle 的表类型报 TS2883（类型无法被命名）。
  这是 emit 选项而非类型检查开关，`tsc --noEmit` 仍然完整运行。

## 6. 排除模式会向下匹配，误删 node_modules 内容

现象：容器反复重启，日志报
`No driver (HTTP) has been selected. ... please, install the "@nestjs/platform-express" package`。

这条错误是**误导性的**——NestJS 在加载 HTTP 适配器时 catch 掉了真实异常，只转述了「包没装」。
而 `require.resolve('@nestjs/platform-express')` 照样成功，符号链接也完好。

直接 `require` 才拿到真话：`Cannot find module './storage/disk'`，来自 `multer/index.js`。

排查发现**两处独立的问题都在干同一件事**，必须都修：

1. **打包层的 `tar --exclude`（真正的根因）**。`deploy.sh` 里
   `--exclude='./storage'`——本意是排除媒体输出目录——在 macOS bsdtar 下**在任何深度都匹配**，
   于是 `node_modules/.pnpm/multer@2.4.0/node_modules/multer/storage` 一起被打包排除。
   已用独立 tar 测试证实。`./data`、`./docs` 同理。
   处理：**改用白名单显式列出要打包的条目**，不做任何模式匹配。

2. **`.dockerignore` 的未锚定模式**。`storage` 不含斜杠时匹配任意层级，
   同样会吃掉 `multer/storage`。处理：全部加前导斜杠锚定（`/storage`、`/data`、`/docs`）。

实测受影响目录共 8 处，还包括 `node-gyp/gyp/data`（那会让原生编译失败）。

教训一：框架转述的错误信息不可信，排查时直接 `require` 取原始堆栈。
教训二：排除模式的"任意深度匹配"是 macOS tar 与 Docker 共有的语义陷阱，
凡是要保留目录树，白名单比黑名单可靠。

## 7. `assets.file_path` 存绝对路径，项目一搬就全 404

**现象**：项目从 `playground/kestrel-studio/` 搬到 `CC/kestrel-studio/` 之后，
历史会话里的图片和视频全部打不开——下载端点对**属主本人**也返回 404。

**根因**：`media.service.store()` 用 `join(resolve(STORAGE_DIR), 文件名)` 写盘，
把这个**绝对路径**原样存进 `assets.file_path`。文件跟着仓库一起搬到了新目录，
行里的路径却还指向旧目录，于是 `res.sendFile` 一律 ENOENT。
（`storage/` 在服务器上是 bind mount，容器路径 `/app/storage` 从不变化，所以在线上一直没暴露。）

**处理**：把"文件在哪"从数据里挪回运行时——`file_path` 只存**文件名**，
读取时用 `src/media/asset-path.ts` 的 `resolveAssetPath()` 按当前 `STORAGE_DIR` 展开。
旧行里那种绝对路径**仍然认**：文件还在原处就原样用（线上不必迁移），
不在就退化成"按文件名在新目录找"——这一步正好修复了上面那 5 条失效记录。

**教训**：凡是把**环境相关的绝对路径**写进数据，就埋了一颗"换目录/换机器即失效"的雷，
而且失效面是沉默的（404 而不是报错）。要持久化的是**相对标识**，路径在读取时解析。
判据很简单：这个值换一台机器还成立吗？

## 8. 语音客户端漏发 `start`：两端都不出声的故障

**现象**：线上按住说话永远停在「正在连接…」，控制台零错误零警告。

**根因**：`web/scripts/voice.ts` 的 `ensureSocket()` 设了 `onmessage` 与 `onclose`，**漏了 `onopen`**，
于是客户端连上 WS 之后什么都不发。而服务端只在收到 `start` 帧时才开会话（建上游会话 → 回
`ready`），于是：socket 是健康的、服务端不知道有通话、浏览器 `ready` 永远为 false、
状态永远停在「正在连接…」。

**为什么难查**：**两端都不进错误分支**。客户端没出错（socket 好的），服务端的 `receive()` 里也
没有"什么都没收到"这条路径。于是容器日志一片空白，看着像"什么都没发生"。

**定位靠外部证据**：1Panel 站点访问日志里 `GET /api/voice?ticket=…` → `101` 且
`$body_bytes_sent = 2`——整条会话只发出了一个 WS 关闭帧；对照 14:16 那次成功通话是 `513123`
（音频下行）。`101` 排除了代理，`2` 才指出根因。

**处理**：客户端在 `onopen` 里发 `start`；服务端补上 `Voice session opened / ended` 与会话建立
失败的 warn（这次难查就是因为成功路径完全不出声）。回归测试断言的是**帧序列**而不是"没报错"，
见 `web/scripts/voice.test.ts`。

**教训**：协议里定义的双向帧，两端各断言一次。**只验"控制台干净"会放过静默失败**——这次页面从
外观到控制台都与正常一模一样。查这类故障时要找的是"应该发生的字节没发生"，而不是"报了什么错"。

**同一族的第二个坑（同日）**：上游对**未提交音频**有 30 秒上限，超了就对后续每一帧 `append` 都回一次
`Input audio buffer exceeded maximum duration (30s)`。客户端的 43ms 分帧意味着**每秒约 23 条错误**，
页面直接刷屏；而中继当时只把上游消息转发给浏览器、不记日志，所以服务端一条痕迹都没有。
处理：单段按住截在 25 秒自动提交（`MAX_HOLD_MS`）、收到 error 后本次按住停止送音频、中继在拒绝后
不再往缓冲区灌、一次失败只往页面发一行。教训与 §8 开头同一条：**上游的报错必须落日志**——
它的原话就是根因本身。
