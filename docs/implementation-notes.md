# kestrel-studio 实现记录（环境踩坑）

> 只增不减的台账：搭建过程中实际撞到并解决的问题，留档以免重装时重复排查。
> 部署环节的坑（构建、端口、反代）见 [deployment.md](deployment.md)。
>
> **路径口径（2026-11-02 目录拆分）**：条目按发生时的目录书写，`src/`、`__tests__/web/`、
> `client/`、`proto/`、`md/`、根目录的 `vite.config.mts` / `tsconfig*.json` 现分别位于
> `server/src/`、`web/__tests__/`、`web/client/`、`crates/proto/`、`crates/md/`、`web/` 与
> `server/`。根目录仍保留 `package.json`、`pnpm-lock.yaml`、`vitest.config.mts`。
> 现行结构见 [architecture.md](./architecture.md) §9。

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
见 `__tests__/web/voice.test.ts`。

**教训**：协议里定义的双向帧，两端各断言一次。**只验"控制台干净"会放过静默失败**——这次页面从
外观到控制台都与正常一模一样。查这类故障时要找的是"应该发生的字节没发生"，而不是"报了什么错"。

**同一族的第二个坑（同日）**：上游对**未提交音频**有 30 秒上限，超了就对后续每一帧 `append` 都回一次
`Input audio buffer exceeded maximum duration (30s)`。客户端的 43ms 分帧意味着**每秒约 23 条错误**，
页面直接刷屏；而中继当时只把上游消息转发给浏览器、不记日志，所以服务端一条痕迹都没有。
处理：单段按住截在 25 秒自动提交（`MAX_HOLD_MS`）、收到 error 后本次按住停止送音频、中继在拒绝后
不再往缓冲区灌、一次失败只往页面发一行。教训与 §8 开头同一条：**上游的报错必须落日志**——
它的原话就是根因本身。

## 9. jsdom 没有 <dialog> 的 showModal/close

**症状**：给历史会话的二次确认换成 `<dialog>` 后，`__tests__/web/main.test.ts` 里凡是走到
`showModal()` / `close()` 的用例全部抛 `TypeError: deleteDialog.close is not a function`，
而且是**未捕获异常**——它逃出用例，直接把整个文件的其他测试一起拖挂。

**根因**：jsdom 有 `HTMLDialogElement` 这个类，也反射 `open` 属性，但**没有实现 top layer**，
所以 `showModal` / `close` 两个方法压根不存在。实测：

```bash
node -e "const {JSDOM}=require('jsdom'); const d=new JSDOM('<dialog></dialog>');
const el=d.window.document.querySelector('dialog');
console.log(el.constructor.name, typeof el.showModal, typeof el.close, 'open' in el);"
# HTMLDialogElement undefined undefined true
```

**为什么以前没撞上**：页面上原有的两个 `<dialog>`（图片预览、下载弹窗）在这套测试里从没被打开过，
缺的是这两条路径的覆盖，不是 jsdom 突然变了。

**处理**：`__tests__/web/test-setup.ts` 里补最小实现，与既有的 `scrollIntoView` 同一位置、同一理由
——**脚本本身不留测试专用的分支**。只镜像脚本真正观察的两件事：`open` 属性与 `close` 事件，
于是「对话框是否被关掉」在用例里依然可断言。`showModal` 返回 Promise（Chrome 的就是），只是没人 await。

**教训**：测试环境缺的是**能力**不是**版本**。用到一个新 DOM 接口前，先确认 jsdom 有没有它，
比等一个未捕获异常把整个文件带走便宜得多。

## 10. 离线 registry：不是"少一个库"，是少一串

**症状**：`cargo check --offline` 在换依赖时报
`no matching package named \`axum-macros\` found … note: offline mode (via \`--offline\`) can
sometimes cause surprising resolution failures`；`tokio-rustls` 则能解析但**运行时**没有
可用 crypto provider。

**根因**：本地 registry 只有历史项目下载过的 crate。缺的不止 `bcrypt`，还有
`tungstenite` / `tokio-tungstenite`、模板引擎（`handlebars`/`tera`/`minijinja`）、
`tower-sessions`、`tower-http` 的 `fs` 特性所需的 `http-range-header`、`axum` 的
`macros` 特性所需的 `axum-macros`，以及 rustls 默认 provider `aws-lc-rs`。

**处理**：缺件在本仓库内实现（对照表见 architecture.md §13.1）；两处必须显式写死
的非直觉配置：

```toml
axum = "0.8"                       # 不带 features = ["macros"]，否则离线解析失败
tokio-rustls = { version = "0.26", default-features = false,
                 features = ["ring", "tls12", "logging"] }   # aws-lc-rs 不在缓存
```

顺带两条推论：axum 的 `ws` 特性依赖 `tokio-tungstenite`，所以**升级不能走框架
抽象**；但 `hyper::upgrade::CanUpgrade` 是 `impl<B> for http::Request<B>`，对 body
泛型，因此在 axum handler 里就能接管裸 socket——先前"没有 tungstenite 就做不了
WebSocket"的结论是错的，错在把框架的便利层当成了唯一入口。

## 11. 漏设 `CARGO_TARGET_DIR`，524 MB 构建产物差点进公开仓库

**症状**：`git status --porcelain -uall` 突然多出 2111 个未跟踪文件；`du -sh -S rust/target`
约 524 MB。`git add rust && git push` 会把它连同提交推进**公开**仓库，历史只能靠
force push 抹。

**根因**：一个清理警告的脚本里用 `subprocess.run(["cargo","check",…])` 调 cargo，
**没有带 `CARGO_TARGET_DIR`**——我这一整个会话其余时候都指到 `/tmp`，唯独这里漏了。
`.gitignore` 当时也还没有 `rust/target/` 这一行。

**处理**：删目录；`.gitignore` 加 `rust/target/`（注意这是目录型模式，路径不存在时
`git check-ignore rust/target` 不命中，其内部路径才命中，别被这个"没生效"的假象骗）；
Makefile 的 `rust-*` 目标统一用 `--manifest-path`，不靠 `cd`。提交时改用
`git add -u <path>`，只暂存已跟踪文件。

**同批教训**：那个脚本还一度"看起来成功"——它读的是 `subprocess.run(...).stdout`，
而 **cargo 的诊断在 stderr**，于是解析不到任何目标、循环空转；紧接着
`grep -c "^warning"` 读到的是缓存构建的汇总行，报出"只剩 1 条告警"。两处假信号
叠在一起，差点让我把"什么都没做"写成"清理完成"。**退出码 0 不等于目标达成**：
验证要看被改的东西真的变了（`git diff`），不要看命令有没有报错。

## 12. 按编译器给的行号改源码，三次踩同一个坑

`cargo` 报 `unused imports: A and B` 时给的行号指向**整条 `use` 语句**，而不是那两
个名字。三次后果：

1. 直接删该行 → 同一花括号里**仍在用**的 `TaskKind` 被删掉，编译失败。
2. 改正则后 `use axum::http{X};` → 因为 `(.+?)::\{` 把 `::` 留在捕获组外，
   重组时丢了分隔符；再补时又用 `(use [\w:]*?)\{` 不判存在，产出 `::::`。
3. 更早一次用 `rstrip('}')` 收尾一段补丁，把 `mod tests` 的闭合括号也削掉。

**处理**：改源码以 `apply_patch` 为先；必须脚本化时，操作单位是**标识符**（在名字
集合上删），并在每次改动后跑一次编译确认；`apply_patch` 对被外部写过的文件会失配
（报"找不到行"而磁盘上那行原样存在），此时先重读文件再改。

## 13. web-sys 的 feature 门与 CLI 版本配对（2026-10-30，WASM 探测）

把客户端改成 Rust→WASM 时撞到的三类"不是缺能力、是缺开关"的坑：

1. **`element.dataset()` 报 no method**。DOM 里 dataset 挂在 HTMLElement 上，
   但 web-sys 把返回类型 `DomStringMap` 放在同名 feature 后面——不加
   `features = ["DomStringMap"]` 就没有这个方法。同理 `Navigator`、`MediaDevices`、
   `AudioWorklet` 都要逐个显式开。**没有编译错误提示告诉你该开哪个 feature**，
   只会报 no method，容易误判成 web-sys 不支持。
2. **`media_devices()` 返回 `Result` 而不是 `Option`**（非安全上下文会抛），
   写 `.ok_or_else()` 直接编译失败。WebIDL 的可抛接口在 web-sys 里一律是 `Result`，
   不可抛的才是 `Option`——按签名读，别按直觉写。
3. **联合类型参数走 `JsValue`**：`MediaTrackConstraints::set_channel_count(&JsValue)`
   收 `&JsValue`，`MediaStreamConstraints::audio()` 已废弃、改用 `set_audio(&JsValue)`
   （返回 `&mut Self`，不能再当表达式链式赋值，否则 E0308）。

另外一条工具链账：**wasm-bindgen 的 CLI 与 crate 版本必须配对**。homebrew 的
`wasm-bindgen` formula 停在 0.2.111 且已标 deprecated，而 `Cargo.lock` 解析到
0.2.129，用 brew 那个 CLI 生成 glue 会在版本检查上失败。正解是装 `wasm-pack`
（按 `Cargo.lock` 自取匹配 CLI）或 `cargo install wasm-bindgen-cli --version <锁定值>`。

还有一条**去不掉的 JS**：`AudioWorkletProcessor` 子类必须在 worklet 全局作用域里
`registerProcessor`，那里没有 DOM、也拿不到 wasm 宿主，所以语音采集那 20 行 JS
是硬地板（详见 verification.md §10.2）。要"仓库里零 JS 源码"只能放弃 AudioWorklet，
改回已废弃的 `ScriptProcessorNode`——那也是 JS。

## 14. 手写 zig 垫片交叉编译 musl：C 编得动，链接过不去（2026-10-30）

`make rust-build-linux` 的第一版实现是四个 `scripts/cross/x86_64-linux-musl-*` 垫片，
把 `cc` crate 与 rustc 找的编译器名转给 `zig cc`。C 侧确实通了（`ring` 的 C/asm、
`libsqlite3-sys` 的 bundled SQLite 全过），死在最后一步链接，两个独立原因叠加：

1. **rustc 在 macOS 主机上默认拿 `cc` 当链接器**，于是走 Apple 的 ld：
   `ld: unknown options: --as-needed -Bstatic -Bdynamic --eh-frame-hdr -z --gc-sections --strip-debug`。
   musl 的链接步骤吐的是 GNU 风格参数，Apple ld 一个都不认。
2. 用 `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER` 指名垫片之后，改成
   **两套 musl 启动文件同时进链**：`multiple definition: _start_c` ——
   zig 的驱动按自己的 sysroot 塞了 `crt1.o`，rustc 的 self-contained 目录又塞了
   `rcrt1.o`。rustc 传的 `-nostartfiles` 没能拦住 zig 那一份。

**处理**：删掉垫片，改用 `cargo zigbuild`（它做的正是这层翻译，含启动文件的取舍）。
一次通过：`ELF 64-bit LSB executable, x86-64, statically linked, stripped`，9.2 MB。

教训：`cc` crate 只吃"换个编译器名"，链接器吃的是**参数方言与 sysroot 归属**——
这两件事不是一个抽象层，垫片只能覆盖前者。手写版本可以止步于"C 编译通过"，
但它给不出能跑的产物。

## 15. 工具链下载的两条实测账（2026-10-30）

| 源 | 实测 |
|---|---|
| ghcr.io（brew bottle 默认源） | **~8 KB/s**：zig 的 37 MB 跑了 20 分钟没完 |
| 清华 `HOMEBREW_BOTTLE_DOMAIN` | 分片测得 **~480 KB/s**，整包安装 8m36s 完成（含 lld@22 依赖） |
| `static.rust-lang.org`（rustup 组件） | ~50 KB/s：wasm32 std 15 MB 用了约 4 分钟 |
| rsproxy.cn（`RUSTUP_DIST_SERVER`） | musl std 十几秒 |
| crates.io（稀疏索引 + .crate） | 可用：`cargo add tera --dry-run` 秒级；web-sys 3 MB 级依赖约 40 秒 |

`brew install cargo-zigbuild` 会拖 `llvm@22` + `lld@22`（数百 MB 量级），在 ghcr 那个
速度下不可行；`cargo install cargo-zigbuild --locked` 源码编译 6m44s，只依赖已装好的 zig。

另：brew 的 `wasm-bindgen` formula 停在 0.2.111 且已标 deprecated，而 `Cargo.lock`
解析到 0.2.129——CLI 与 crate 版本必须配对，别用 brew 那个（见 §13 末与 verification.md §10.3）。

## 16. wasm-pack 会静默 cargo-install 它的 CLI（2026-10-30）

`wasm-pack build` 第一次跑时不下载预编译 CLI，而是**当场 `cargo install wasm-bindgen-cli
--version <锁定值>`** 到 `~/Library/Caches/.wasm-pack/.wasm-bindgen-cargo-install-0.2.129/`。
在这台机上 13 分钟没结束，而且**全程没有任何输出**——看起来像卡死。判据：

```bash
ls ~/Library/Caches/.wasm-pack/            # 出现 .wasm-bindgen-cargo-install-<版本> 即在建 CLI
find <target> -newermt '-3 minutes' -type f # .wasm 已产出则说明 cargo build 段已完成
```

`--release` 的 `.wasm` 在 CLI 之前就已生成（1.2 MB / gz 303 KB，见 verification.md §11.5），
所以体积数字不必等 CLI 编完。要省时间可以先把 CLI 显式 `cargo install wasm-bindgen-cli
--version 0.2.129` 装好，wasm-pack 就会直接用。

两条后续事实（同日补）：

1. 那次构建**总共 11m31s**，CLI 编译完接着默认跑了 `wasm-opt`，最终产物是 **222,694 B /
   gz 95,243 B**。所以"1.2 MB / 303 KB gz"是 wasm-opt **之前**的中间产物，不能当交付体积引用
   ——我第一轮就是这样误引了它（见 verification.md §11.5 的更正）。
2. CLI 编好后缓存在 `~/Library/Caches/.wasm-pack/.wasm-bindgen-cargo-install-0.2.129/`，
   之后同一版本的 `wasm-pack build` 只要 ~1 秒。

还有一条**关于体积实验的做法**：要单测某个依赖的重量，别改仓库，`cp -R` 出
`client/` 与 `proto/` 到 `/tmp` 后在副本上删依赖再构建——路径依赖 `../proto` 是按
manifest 所在目录解析的，只拷 `client/` 会得到 `unable to update /private/tmp/proto`。

另外记两条**环境层面的观测**（原因未知，只记现象与对策）：本轮出现过两次"写成功但文件不在"——
`proto/` 整个目录在某一步消失（`apply_patch` 报成功、随后 `find` 查不到），以及
brew 报 `wasm-pack ... 10 files, 6.4MB` 安装成功后 `/opt/homebrew/Cellar/wasm-pack/` 却是空的、
`/opt/homebrew/bin/wasm-pack` 成断链。两处都不是脚本删的（无 rm 记录），也没有并发进程在动这些路径。
对策很笨但有效：**新建文件或装完工具后立刻独立验一次**（`find` / `--version`），
别把工具的退出码当存在性证明；`cargo test` 报 "failed to load source ... No such file"
就是这种消失的第一个信号。

## 17. 目录拆成 monorepo：三处"路径按 cwd 解析"的暗坑（2026-11-02）

把 `src/` 挪进 `server/`、`client/` 挪进 `web/client/`、`proto/` `md/` 挪进 `crates/`，
代码语义一行没改，坏的全在**按工作目录解析的路径**上。三条都记下来，因为它们都不会
在 `tsc` / `cargo check` 里报出来。

| 现象 | 根因 | 处理 |
|---|---|---|
| 若把构建命令改成 `cd server && nest build`，本地跑起来会打开一个**空数据库**，`.env` 也不生效 | `loadEnvFile()`、`DATABASE_FILE`、`STORAGE_DIR` 全按 `process.cwd()` 解析，cwd 一挪语义就变 | **cwd 一律留在仓库根**，改用 CLI 自己的配置指针：`nest build -c server/nest-cli.json -p server/tsconfig.json`、`drizzle-kit push --config server/drizzle.config.ts` |
| `.hbs` 模板被拷到仓库根 `dist/` 而不是 `server/dist/`，页面 500 | `nest-cli.json` 里 `assets[].outDir` 与 `sourceRoot` 都是**与 cwd 拼接**的；原来那句 `"outDir": "dist"` 只是恰好等于编译器的 outDir，换个布局就成了错的绝对目标 | `sourceRoot` 写 `server/src`；`assets` **删掉 `outDir`**，让它沿用 tsconfig 的 `server/dist`。顺带一条：`nest-cli.json` 由 `JSON.parse` 读取，**写注释会让构建直接失败**（tsconfig 才是 JSONC） |
| `Failed to lookup view "chat" in views directory ".../src/view/views"`，两个测试文件整文件崩掉 | `server/__tests__/{view,admin}/*.controller.test.ts` 里 `VIEWS_DIR = join(process.cwd(), 'src', 'view', 'views')` 把源码位置写死成"仓库根有 src/"；构建产物路径由 `__dirname` 推导所以没事，**只有测试按 cwd 拼** | 抽成 `server/__tests__/views-dir.ts`，锚在 `import.meta.url` 上而不是 cwd 上，两处共用 |

还有一条**关于验证顺序**的教训，比上面三条更值钱：第一次跑 `make test` 报的是
`listen EPERM: operation not permitted`（沙箱禁止 bind socket），6 个文件全死在 bootstrap，
`VIEWS_DIR` 那个真故障被完全遮住——因为那两个文件在能监听端口之前就不会走到渲染。
同一批测试在允许 bind 的环境里跑出 43 个失败才是真信号。**结论：环境层报错会冒充成
"改动没破坏东西"或"改动破坏了一片"，先看报错是不是 EPERM/网络/权限，再决定信不信这个结果。**

本次拆分实跑过的证据（同一天）：`tsc --noEmit` 三份配置全过；`vitest run` 30 files /
**510 passed**（含 6 个会真起 HTTP 监听的文件）；`cargo test`（rust）**161 passed / 0 failed / 2 ignored**，
其中三条契约测试是运行时去读 `../web/scripts/main.ts`、`../web/client/src/voice_page.rs`、
`../web/client/src/chat_page.rs` 的，能过就说明新路径在 cargo 的 CWD（package 根）下成立；
`make clean && make build` 后产物落位 `server/dist/main.js` + `server/dist/view/views/*.hbs`（8 个模板 +
partials）与仓库根 `public/assets/{main.js,main.css,voice.js}` + `public/{capture.js,logo.svg}`。
**仍未验证**：服务真跑起来后的浏览器控制台（本机禁止起服务）。

### 17.1 同日第二步：Node 侧换成真 pnpm workspace

第一步刻意没做的事——把依赖从根 `package.json` 下沉到 `server/` 与 `web/`——代价是一次
`pnpm install`：重写根 `pnpm-lock.yaml`（全仓只有这一份锁）并重装 `node_modules`，需要出站网络。
目录形状稳定之后再付这笔成本，拆法变成：根清单不声明依赖、只留 `pnpm -r build|typecheck|test`
三条聚合脚本，`vitest.config.mts` 也拆成两份 app 内配置（根不再留测试编排文件）。

**nest 脚本为什么开头是 `cd ..`**：pnpm 执行 `package.json` 脚本时 cwd 恒为该 package 目录，
而本项目的 `.env`、`DATABASE_FILE`、`STORAGE_DIR` 按 cwd 解析——直接在 `server/` 里 `nest start`
会静默打开一个 `server/data/` 下的空库。所以 `server/package.json` 的 nest / drizzle 脚本先
`cd ..` 再带 `-c server/nest-cli.json -p server/tsconfig.json`。这条路能走通的前提是：pnpm 在
启动脚本前就把 `server/node_modules/.bin` 注入 PATH，`cd` 之后二进制仍然可解析；反过来若把
`@nestjs/cli` 留在根、命令也留在根，虽然省掉 `cd`，但依赖归属又回到根，P4 删 Node 栈时删不干净。
`vite` / `tsc` / `vitest` 没这个约束（配置与路径都相对自身解析），因此在本 app 目录直接跑。

**拆测试 runner 后要数总数，不能只看"跑过了"**：include 模式从 `server/__tests__/**`（根 runner
里）变成 `__tests__/**`（app 内 runner）时，写错一个层级不会报错，只会少跑文件——它会安静地
变成 "No test files found"，或者更糟：安静地跑了一部分。本次核对的是**用例总数守恒**：
拆前单 runner 30 files / 510 tests，拆后 `server` 28 files / 461 tests + `web` 2 files / 49 tests
= 510，一条没丢。两轮 runner 的代价约 1 秒启动开销。

复跑证据：`pnpm install`（重写锁）、`make typecheck`（两个 app 全过）、`make test`
（461 + 49）、`make clean && make build`（`server/dist/main.js` + `server/dist/view/views/*.hbs`、
仓库根 `public/assets/*` + `public/{capture.js,logo.svg}`）全绿；Rust 侧本轮零改动。

## 18. 单独构建 `crates/*` 会就地留下没人跟踪的 Cargo.lock（2026-11-02）

Rust 侧没合并 workspace（四棵 crate 各自解析），而仓库只跟踪两份锁文件：

```
$ git ls-files | grep Cargo.lock
rust/client/Cargo.lock
rust/server/Cargo.lock
```

`crates/proto` 与 `crates/md` 没有 lock——平时它们是作为路径依赖跟着上面两棵 crate 一起解析的。
但一旦单独对它们跑 cargo，cargo 就按"这是一个包的 workspace"就地写一份：

```
$ cargo test --manifest-path crates/proto/Cargo.toml
$ git status --porcelain=v1
?? crates/proto/Cargo.lock        # 未跟踪，.gitignore 里也没有 cargo 规则
```

危险不在文件本身，而在它出现的方式：它是一次**验证动作**的副产品，看起来跟改动无关，于是
`git add -A` 会把它连同真正的改动一起提交进去——那是一份没人复核过的依赖锁。

**处理**：要么别单独跑 `crates/*`（改动 `crates/proto` 时，跑 `rust/server` 或 `rust/client`
的测试就足以覆盖，两边都把它当路径依赖吃进去），要么跑完就删掉那份 lock。本次删的就是它。

**当时没动 `.gitignore`**：加一条 `crates/*/Cargo.lock` 是把这次的疏忽固化成规则，而不是修掉来源；
按项目约定 `.gitignore` 只由用户明确要求时才改。
**2026-11-03 收尾**：用户明确要求后加了 `crates/*/Cargo.lock`，本节的坑从此结构性关闭——复现方式是
`cargo check --manifest-path crates/md/Cargo.toml` 之后 `git status` 仍然干净。同一轮里还修了
`.gitignore` 里一条旧 target 名（`make build-rust-client` → `make build-wasm`，改名发生在 `b9db776`）。

## 19. `dataset()` 的键是 camelCase，写成属性的 kebab 拼法是静默 None（2026-11-06）

`HTMLElement::dataset()` 返回的是 `DomStringMap`，它的键是 **JS 属性名**，也就是 camelCase：
属性 `data-conversation-id` 要读成 `dataset["conversationId"]`。在 Rust 侧写成
`body.dataset().get("conversation-id")` 不报错、不 panic、也不返回 `None` 之外的任何东西——
它就是个空读取，于是每个调用点都走 `None` 分支，页面照常渲染，功能照常坏。

WASM 客户端一次就写了六处（`rust/client/src/chat_page.rs`，2026-11-06 之前）：

| 读取 | 正确键 | 静默失效的后果 |
|---|---|---|
| `conversation-id` | `conversationId` | 已有会话里提交一律新建会话；首屏不订流（`if let Some(id)` 永不成立）；历史行 active 恒空 |
| `start-seq` | `startSeq` | 订流游标恒空（被上一行掩盖） |
| `message-id` | `messageId` | SSR 已画的轮次不进 replay 去重集合（被第一行掩盖，一旦订上流就会重复画） |
| `delete-conversation` | `deleteConversation` | 历史行的删除按钮认不出是哪一行 |
| `delete-choice` | `deleteChoice` | 删除确认对话框的确认/取消都不响应 |
| `can-delete-conversations` | `canDeleteConversations` | 删除控件对所有账号都不渲染——连普通用户也一起没有 |

**为什么两道质量门都没拦住**：`cargo test`（宿主目标）根本编不到 `chat_page.rs`，它是
`cfg(target_arch = "wasm32")` 门后的 DOM 模块；`rust/server/src/view/tests.rs` 的契约测试钉的是
**元素 id**，不是**属性键**。这正是 roadmap.md §4.1 当时写下的真实代价——"客户端从此没有类型检查
与 jsdom 测试之外的运行时护栏"——第一次以它预言的方式兑现。上一轮 jsdom 套件退役时也没留下等价物。

**结构性关闭**：`rust/client/src/dataset_keys.rs`（`#[cfg(test)] mod`，不进任何包）扫五个 DOM
模块的源码，断言每个 `dataset().get("…")` 的实参不含 `-` 与 `_`，并且扫描条数 `< 8` 就报"解析器过期"，
避免护栏自己变成假人。复现方式：把任一键改回 kebab 拼法，`cargo test` 立刻红。
反向验证做过了：把 `conversationId` 改回 `conversation-id` 后，报错原文是
`…/chat_page.rs: `dataset().get("conversation-id")` at byte 10014 uses the attribute spelling`——
文件、字节偏移、错键与"该用 camelCase"都在。还原后 39 条全绿。

**同类写法不受影响**：`set_attribute("data-conversation-id", …)` 与 `get_attribute("data-…")` 走的
是属性名，本来就是 kebab，两侧一致。要记住的分界线只有一条——**经过 `dataset()` 就是属性名，
绕过它就不是**。

## 20. 沙箱里跑 `cargo test` 会假红两条 storage 测试（2026-11-06）

`make test` 的最后一半在受限沙箱里跑时：

```
test result: FAILED. 167 passed; 2 failed
admin::tests::removing_an_account_takes_its_rows_and_its_files  --- PermissionDenied ("Operation not permitted")
http::tests::assets_follow_ownership_and_the_signature_is_the_only_authority --- 同一处
```

两条都在**创建 storage 临时目录**那一步炸（`src/admin/tests.rs:227`、`src/http/tests.rs:478`），
与被测逻辑无关。同一份代码在沙箱外是 **169 passed / 0 failed / 2 ignored**。

**为什么单独记一条**：这类假红很容易被骗成"改动弄坏了什么"，然后引发一次不必要的回滚或返工。
判据很简单——**失败信息是 `PermissionDenied` 且发生在建目录/写临时文件那步**，就先按环境问题处理，
用完整权限复跑一次同一套门再说。Nest 侧（vitest 用 in-memory 或仓库内的临时库）不吃这个亏，
`nest/server` 28 files / 461 与 `nest/client` 49 在沙箱内外都绿。

## 21. 沙箱里 `pnpm --dir nest/server test` 假红 6 个控制器测试文件（2026-11-07）

```
Test Files  6 failed | 23 passed (29)
     Tests  336 passed | 139 skipped (475)
Error: listen EPERM: operation not permitted 0.0.0.0
```

红的是 `admin.controller` / `auth.controller` / `guards` / `view.controller` /
`asset-frame.controller` / `chat.controller` 六个文件，共同点是里面有一句 `await app.listen(0)`：
它们真起一个监听套接字，而受限沙箱不允许 bind，于是**整个文件在加载阶段就炸**，里面 139 条一条没跑
（计入 skipped，不是 failed）。`grep -l "app.listen(" nest/server/__tests__` 的命中数正好是 6，
这条计数本身就能当判据用。

判据与 §20 是同一条族：**报错是 `EPERM` 且发生在 `listen()` / 建临时目录那一步**，先按环境问题处理，
要全绿就换到允许 bind 的环境复跑同一套门。**不要去 mock 掉 `listen()`**——那六个文件测的正是真实
HTTP 面上的重定向、状态码与 cookie，把它们改成不起服务就等于把测试改成不测那件事。

**这条同时更正 §20 末句**。那句「Nest 侧不吃这个亏，28 files / 461 在沙箱内外都绿」只对**文件读写**
那一类成立，对**端口**不成立：今天的计数也对不上，475 - 461 = 14，正是这次新增的
`access-log.test.ts`（6 条）与 `configuration.test.ts` 里 `ACCESS_LOG` 那组（8 条）。所以要么当年那
461 是在允许 bind 的环境里跑的，要么那六个文件当时没进同一轮——两种解释都说明**别把 §20 那句当保证
引用**；跑 nest 全量之前先确认自己在不在沙箱里，需要的是「文件级判据」而不是「一条都没红」的印象。

## 22. Nest 管线重新上线：两处脚本首跑即错，一条构建假设被推翻（2026-11-07）

**其一，白名单漏了 `docker-compose.yml`。** `scripts/deploy-nest.sh` 照抄 Rust 那套「显式白名单」时
只放了 `Dockerfile.nest` 与 `.dockerignore`，于是宿主机上的 compose 还是上一次 Rust 部署上传的那份，
里面**没有 `nest` service**，构建直接 `no such service: nest`。判据很干脆：**只要新 service 是这次
加的，compose 文件本身就必须进同一次上传的上下文**——否则仓库里的定义和宿主机上的定义不是同一份。

**其二，`NEST_HOST_PORT='\${NEST_HOST_PORT:-8849}'` 这个写法是错的。** 本意是"把默认值留给远端展开"，
实际经 ssh 的参数拼接传到远端成了字面量串，compose 拿它当地址解析，报
`invalid IP address: ${NEST_HOST_PORT`。两重多余：compose 自己对 `${VAR:-default}` 做插值，不需要
外面喂；而**构建阶段根本用不到端口**。删掉前缀即可。教训是别在 ssh 的引号里再套一层转义去"预展开"
别人家的模板语法。

**其三，旧 Dockerfile 的 golang 原生编译阶段已经是死件。** 那条
`npm_config_nodedir=/usr/local node-gyp rebuild` 是从拆分前的管线抄来的（当时 better-sqlite3 需要
在 Linux 上重编）。v13 把八个平台的 prebuild 打进包里（`…/better-sqlite3/prebuilds/linux-x64.node`
等），require 时按平台挑：实测那次 rebuild 花了 **1.8 秒**、`gyp info ok`，但
`build/Release/` 里只有空的 `obj.target`，**没有 `.node`**——因为压根没东西要编，而应用照样开库。
判据：**看 `build/Release/*.node` 在不在，别信 `gyp info ok`**；1.8 秒编不出 SQLite。据此
`Dockerfile.nest` 改成单阶段，镜像从历史那版的 1.27 GB 降到 534 MB，宿主机也不再需要 `golang:1.24`
这一层。deployment.md §1.1 那三条"盘上残留不能直接用"的告诫随之作废（现在每次都由
`deploy-nest.sh` 按当前布局重新打包上传）。

## 23. 「丢掉 `?c=` 刷新就不会画回来」是错的前提：回落不由 query 决定（2026-11-08）

症状是用户报的：点「新会话」，内容区清了；**刷新一下浏览器，那条会话又完整地在内容区出现**。

第一反应是找缓存。没有缓存可找：`localStorage` 全仓只有一个键 `kestrel-studio:mode`
（`nest/client/scripts/main.ts` 与 `rust/client/src/chat_page.rs` 的 `MODE_STORAGE_KEY`），存的是模式
选择；SSR 的 HTML 响应不带 `Cache-Control`（只有静态资源带 `no-cache`）。画回内容区的是服务端
`GET /` 的会话选择规则本身——`?c=` 缺失时回落到「最近更新的那条」。

真正的坑在于**那次修复的注释写着一个不成立的前提**。verification.md §22 那轮走查之后，两个客户端
都把地址栏改成裸 pathname，注释是「Drop ?c=<id>, otherwise a reload re-renders the conversation we
just left」（`resetToNewConversation` / `reset_to_new_conversation`）。清场本身是对的、也确实把 §22
那三条复验过了；但删掉 query 并不能防住刷新：回落不看 query，`/` 与 `/?c=5` 在「5 就是最近一条」时
渲染结果一模一样。也就是说那次提交修的是「这一屏」，用户报的是「下一屏」，两者从没对上过。

判据（下次写这类状态时照它检查）：**一个状态如果只能靠地址栏里某个参数的「缺失」来表达，而这个
缺失又是另一件事的触发条件，那它就重载不保**。要么给它一个自己的名字，要么把默认档改成它。
这里选前者——`?new=1` 两端同形，`?c=` 的既有语义与登录跳转 `302 → /` 的手感都不动。

顺带两条口径：值只认 `1`（跟 `/login?error=1` 一致，`?new` 或 `?new=on` 等同于不带）；创建出新会话
后 `create_conversation` 会把地址栏改写成 `?c=<id>`，所以游标低于保留窗口时那次 `resync → reload`
不会落在空舞台上（空舞台上根本不会有开着的流）。

**缺陷在修复前的线上包上复现过一次**（2026-11-08，三次导航的读数表在 verification.md §29.1：点完
「新会话」`search=""`、`<article>` 归零，刷新后 `search` 仍是 `""` 而 `data-conversation-id` 又变回
`14`；那次 `fetch('/?new=1')` 在线上旧包里仍返回会话 14，正是"标记还没上线"的证据）。**修复后的那一屏
已于同日 17:58 部署后复跑关闭**：刷新停在 `/?new=1` 空舞台，空舞台发一句话又把地址栏换成 `/?c=15`
并回放（读数在 verification.md §29.2 末）。

## 24. 给三处写加所有权：deferred BEGIN、同名索引、两段 DDL 的注释语法（2026-11-09）

§13.2 那三处（启动收尾、任务 worker、journal 的 `seq`）改完之后，值得记的是改的过程中咬人的
几件事。为什么这么改在 architecture.md §13.2，质量门读数在 verification.md §31。

**`pool.begin()` 不是 `BEGIN IMMEDIATE`。** Rust 侧那段「一个事务，因为 `seq` 是按会话算的、
不能两个写者同时算」的注释描述的是一个没有发生的保护：sqlx 发的是裸 `BEGIN`，SQLite 的 deferred
事务在**第一次写**之前不取写锁，读到的是别人的快照，升级时才去看快照变没变——变了就
`SQLITE_BUSY_SNAPSHOT`，整个 publish 回滚。于是「读 MAX 再 INSERT」这两步从来不在同一个临界区里，
跨进程要么落下重复 `seq`，要么丢一帧。
**判据：不要看代码里有没有 `begin()`，要看这条语句什么时候拿写锁。**
最小正确形状也不是「换个更硬的事务」，而是**把下一步的值算在写语句自己身上**：

```sql
INSERT INTO conversation_events (conversation_id, seq, …)
SELECT ?1, COALESCE(MAX(seq), 0) + 1, … FROM conversation_events WHERE conversation_id = ?1
RETURNING seq
```

单条写语句是先取得写锁再执行的，第二个写者**等**（busy timeout）而不是猜，两侧同形，也不需要任何
事务管道。`db.rs` 里那句与实现不符的注释一并改了，别留第二次。

**`CREATE UNIQUE INDEX IF NOT EXISTS` 撞同名是静默空操作。** 那个索引早就以普通形态存在，而
`IF NOT EXISTS` 按**名字**判断，于是这条语句什么都不做——既不报错也不升级，「加了 UNIQUE」这件事在
老库上根本没发生。要升级只能 `DROP` 再 `CREATE`，而 `DROP` 之前必须先处理重复行，否则 `CREATE`
直接失败。所以顺序是：**查重复 → 把每对重复里 rowid 较大的那些挪到该会话编号的尾巴 → 再
drop/create**。
留尾这个选择有理由：`seq` 重复的行本来就**永远不可见**（直播与重放都按 `seq >` 过滤），保留最小
rowid 待在原值上——那是所有游标已经消费过的值——把看不见的行补到末尾，是「这段历史从来没有合法
顺序」这件事破坏最小的读法。修完打一条 `ERROR`（不是 WARN）：那些行刚从不存在变成进了正文。
另一条约束是 **UNIQUE 不写进 DDL 字符串**：写进去就等于给一个已经带重复的库判死刑——建索引失败 →
`apply_schema` 报错 → 服务不起。DDL 里留普通索引，UNIQUE 由迁移函数负责，失败时响但不致命。

**两段同构的 DDL 字符串，注释的语法约束正好互斥。** 这是本次真踩到的坑：
`nest/server/src/database/database.module.ts` 的 DDL 是 **JS 模板字符串**，注释里出现反引号会直接
终止字符串，报一串 `TS1005: ',' expected` 加 `TS1443: Module declaration names may only use ' or "
quoted strings`——错在这条语句，看着却像别处的类型问题。`rust/server/src/db.rs` 的 DDL 是 raw
string，反引号无所谓，但它是 `DDL.split(';')` 逐条执行的，注释里一个分号就把一条语句劈成两条，
第二条以「一句英文注释」开头，启动即失败。
**判据：改这两处 DDL 的注释时，Rust 侧不要分号，Nest 侧不要反引号。** Nest 那边的注释里已写死这条
提醒，免得下次再靠编译器发现。

**写入者身份必须是「每套后端」而不是「每个进程」。** 第一反应是给进程一个随机 boot id，而那正好
毁掉这条改造要保的东西：新进程就是**该**收拾上一个自己没来得及收尾的轮次的那个人。所以用
`rust` / `nest` 两个常量（`common/writer.rs` 与 `common/writer.ts`），前提是每套对同一个库只跑一个
进程——那条前提由「入口按子域固定」守（§13.2 第 4 条），不是偏好。
无主行（`owner IS NULL`）既不能当成「我的」也不能当成「别人的」：它可能来自这张表自己的过去，
也可能来自对面那套还没跑迁移的版本**正在 streaming** 的一轮。所以给它一个宽限期，两端同为 5 分钟
（这个数字是协议不是细节），过期的收尾、没过的不动。

**`attempts` 不进 claim 语句。** roadmap §3 原先写的形状是 `SET status='claimed', attempts=attempts+1`，
真做时两条都没采纳：`status` 不加新值——加了就要 `active()` 的非终态判断、`TaskStatus` 的线类型和
页面标签表一起改，还会让「崩溃时正持有」的任务掉出队列，而租约过期后没人再认领它；`attempts`
更不碰——它计的是供应商失败次数、驱动 `MAX_MIRROR_ATTEMPTS`，而租约每 8 秒续一次，把它放进 claim
等于在任何东西失败之前先跑光那个计数。claim 只需要「一条语句 + 一个条件 + 看 changes()」。

## 25. 宿主机自带 sqlite3 3.7.17：pragma 表值函数的写法直接报语法错（2026-11-09）

部署后想在宿主机上回查活库的 schema，`ssh lavo-test "sqlite3 … 'SELECT name FROM pragma_table_info(x)'"`
这条路**走不通**，报的是 `Error: near "(": syntax error`——看着像引号被 shell 吃掉了，其实是宿主机自带的
3.7.17（2013 年）不认识 **pragma 表值函数**（把 `pragma_xxx(…)` 当表用是 3.16 才有的）。换经典写法就有结果：

```bash
sqlite3 -header data/kestrel-studio.db 'PRAGMA table_info(messages);'
sqlite3 data/kestrel-studio.db 'PRAGMA index_list(conversation_events);'   # 列: seq|name|unique|origin
sqlite3 data/kestrel-studio.db "SELECT sql FROM sqlite_master WHERE name='idx_events_conversation';"
```

两个连带的坑：`pragma_index_list` 的输出里**没有 `sql` 列**（索引定义要从 `sqlite_master` 取）；
而 `sqlite3 data.db "SELECT … FROM pragma_index_list('t') WHERE name='x'"` 里 `WHERE name=` 这种
按列过滤，也同样是 3.16+ 的能力。

要跑复杂查询（窗口、`GROUP BY … HAVING`、CTE）就别在宿主机上凑：`docker compose stop` 之后 `-wal`
与 `-shm` 会消失（干净关闭已 checkpoint），此时 `cp -a` 出来的库文件是一致的，scp 回开发机用 python 的
sqlite **3.53.4** 以 `file:…?mode=ro` 打开查。判据：**先看 `data/` 里 `-wal` 的大小**——0 字节才说明
主文件就是当前全部状态；容器在跑且 wal 非空时只拷主文件会丢掉最近的写入（2026-11-09 那次预检就是靠
这点决定先 stop 再快照，顺手留下 `data/backup-20261109-pre-ownedwrites.db`）。

顺带一条属于 schema 而不是属于工具的事实：**`ALTER TABLE ADD COLUMN` 只能追加在表尾**，所以活库
`messages` 的列序（`owner` 排在 `created_at` 之后）与新建的内存库（DDL 里写在之前）**不同**。两侧所有
SQL 都按列名读写，因此仍然同形；但任何人写 `SELECT *` 再按下标取值，就会在两个库上拿到不同的列。

## 26. 并行冒烟环境的四条运维实情（2026-11-09）

§34 那套「两套同时跑一份库副本」的环境搭起来之前，先撞上四件事。都跟代码无关，但都会让人误判。

**`.env` 里的 `SUPER_ADMIN_PASSWORD` 已经不是凭据了。** 用它登录，两套都回 `302`，看着像成功——
**失败也是 302**，`location: /login?error=1`。所以判据只能读 `Location` 头，不能读状态码：

```bash
curl -s -D - -o /dev/null -X POST -d "username=…&password=…" http://127.0.0.1:8850/login \
  | grep -i '^location'          # location: /  才是登录过了
```

这是既有设计的直接后果：超管口令是 `/admin` 换的，重启不重置（`.claude/skills/deploy/SKILL.md`
§Secrets 那句「a restart never resets it」）。**副作用是宿主机侧任何脚本都不能再靠 `.env` 拿到登录态**。
要在副本上做实测就在副本里建一个一次性普通账号（本次是 `smoke-par`，口令只活在那份副本上），
别去改 `tww316` 的哈希——那是唯一一处会把测试动作溢出到真实账号的地方。
顺带一条互操作事实：**同一个 bcrypt `$2b$` 哈希两套都验得过**（bcryptjs 写的，Rust 的 bcrypt crate 也认），
所以「两套共用一张 `users` 表」在口令校验这一层是通的；不通的是 cookie 签名与 `sess` 行（§13.4）。

**`.backup` 能在生产容器正跑的时候取到一致副本**，宿主机的 sqlite3 3.7.17 也支持它：

```bash
sqlite3 data/kestrel-studio.db ".backup '/tmp/kestrel-parallel/data/kestrel-studio.db'"
```

出来的副本比主文件大（983 KB vs 872 KB），因为它把 WAL 里的内容一并落了盘——**这不是坏副本**。
比 `cp -a` 好的地方在于不用为了拿一致快照去停生产容器。

**`docker run` 复刻 compose 的启动面要带三样**：`--env-file .env`、`-e PORT=<容器内端口>`、
以及 Rust 那侧的 `--tmpfs /tmp`（`scratch` 没有 `/tmp`，SQLite 建临时 b-tree 会失败，而且只在需要
临时页的那条查询上失败，不是启动时）。Nest 那侧基于 node 镜像不需要 tmpfs。忘了 `PORT` 的表现为
「容器 Up、端口映射对、但 502/连不上」，因为进程还在听 compose 默认的那个号。

**冒烟环境跑完就删，别在 `/tmp` 过夜**：`docker rm -f par-rust par-nest` 加
`rm -rf /tmp/kestrel-parallel`。副本里带着真实会话正文与 68 MB 资产，虽然只绑在 127.0.0.1，
留在宿主机的 /tmp 上没有任何理由。删完务必回查生产面那几个数（`live_convs` / `live_events` /
`live_assets` / `live_tasks` 与 `storage/` 文件数）——本次是 16 / 6,827 / 23 / 4 / 23，与拆场前
各次读数逐个相同，这才叫「实测没有溢出到生产」。

**一次真实渲染不必为了测 claim 而烧。** 复用既有 provider 任务号的那次没能形成多 tick 争用（一个 tick
就进终态）；bogus 任务号也不行（供应商回 UNKNOWN 直接被判定终态 `failed`）。真正能证明认领判定的形状是
**预置租约**：插一行 `status='queued', claimed_by=<对方>, claimed_at=<now>`，让只有一套在跑——它连续几个
tick 都不动这一行；再把 `claimed_at` 拨到 6 分钟前，下一 tick 就接管。新鲜/过期两个方向都在真实 worker
上验到了，一次真实生成都没花。

## 27. 哨兵数的是它想到的东西；一次性双跑不必过 cargo（2026-11-09）

两件事，都跟「怎么拿到证据」有关，不是 bug。

**`make census` 这个哨兵有盲区，而且盲区正好在会出事的地方。** 它列了 14 类 markdown 构造，
但把两套渲染器逐构造对跑之后发现：**分歧的 9 类里有 5 类（引用块、分隔线、嵌套列表、有序列表、
ATX 标题）从来没被数过**。原因是那 14 类是按「子集外会不会出现」挑的——一组**担心**的清单，
而不是一组**测过**的分歧。担心会漏，实测不会。所以加哨兵项的正确顺序是：**先双跑量出分歧面，
再把分歧面写进哨兵**，而不是反过来。（读数在 verification.md §36.2/§36.3。）

顺带一条同源的教训：结构一致要用**结构签名**比，别比字节。`marked` 与 `kestrel-md` 在空白和生成的
`id`/`class` 上必然不同，那些不是用户看得见的差异；`href`/`src`/`alt` 要保留，因为那些看得见。
反过来也成立：**比一致也要防"都渲染成空"被当成一致**——所以每条都打印两侧的 token 数。

**一次性把 `crates/md` 编出来双跑，不用 cargo。** 想拿真实的 `kestrel-md` 跑一份外部输入，
路子上有三个坑：写个临时 bin 依赖它会牵动 §18 那条「单独对 `crates/*` 跑 cargo 就地留下没人跟踪的
Cargo.lock」；把渲染逻辑抄进脚本就不是同一个实现了；为了省事跳过对照更是白做。真正干净的做法是
`rustc` 两步，绕开 cargo：

```bash
rustc --edition 2021 --crate-type=lib --crate-name kestrel_md crates/md/src/lib.rs -o libkestrel_md.rlib
rustc --edition 2021 driver.rs --extern kestrel_md=libkestrel_md.rlib -o driver
```

`crates/md` 本身零依赖（每一个字节都要发给用户，`pulldown-cmark` 实测 +87 KB gz，见
verification.md §11.5），所以一条 `--extern` 就够，不需要任何 registry 解析，自然也不产生 lock。

两侧用同一个**行协议**喂：`id|HEX(正文)` 进、`id|HEX(html)` 出——十六进制是为了让一条多行消息在管道
里仍然是**一行**，比 base64 少一次依赖，也比任何分隔符方案都不会和正文打架。marked 那侧同理，
按绝对路径 `require('.../nest/client/node_modules/marked')`，跑法与生产一致
（`marked.parse(src, { async: false })`，默认选项）。

**这类脚手架带的是生产正文，用完就删。** 本次的目录（`~/tmp/md-drift/`，装着从活库导出的 40 条
助手消息）跑完即 `rm -rf`，仓库里只留下能复算的哨兵与读数的记录。判据不是「tmp 目录干净不干净」，
是**真实内容不该在开发机上过夜**。

## 28. marked 的注入面钩子在 renderer 不在 tokenizer；两份配置为什么手抄（2026-11-09）

要「让模型写的原始 HTML 进不了 DOM」，第一反应是拦在词法阶段——**这个反应在 marked 上是错的**。
实测三种写法（marked 18）：

| 写法 | 结果 |
|---|---|
| `tokenizer: { html() { return false; } }` | **毫无效果**。`false` 在 marked 的扩展协议里是「用回默认实现」 |
| `tokenizer: { html() { return undefined; } }` | 同样毫无效果（`<img onerror>` 照旧成元素） |
| `renderer: { html(token) { … } }` | **生效**。marked 对每个 HTML token 调它一次，`token.raw` 是字面源码，`token.block` 区分行级/行内 |

所以正确形状是在 renderer 上转义：块级包 `<p>`（保住段落分隔，且转义后的内容只是文字，`<p>`
套它合法），行内直接转义（不额外造段）。判据：**别信"覆盖 tokenizer 就能拦住"这类直觉，
`use()` 的回落语义是文档里一句话，但只有跑一遍才知道它对自己要改的那条路意味着什么。**

另一条反面教材值得留着：**「先把整段正文转义再喂给 marked」是错的修法**。代码围栏里的
`<div>` 会被转义两次，用户看得见 `&lt;div&gt;`。这条现在钉在
`nest/server/__tests__/common/markdown.test.ts` 里（断言 `&lt;` 不变成 `&amp;lt;`）。

**两份 marked 配置是手抄同步的，不是忘了抽公共。** `nest/server/src/common/markdown.ts` 与
`nest/client/scripts/markdown.ts` 内容同形。想合成一份就要新建一个共享 JS 包，那要动
`pnpm-workspace.yaml` 与 `pnpm-lock.yaml`——而这个仓库的既定口径是「依赖声明在用到它的那棵树里」
且 lockfile 不随手改（CLAUDE.md「拆分的三条口径」）。所以同形复制 + 一根逐字断言把两边钉在一起：
`nest/client/__tests__/markdown.test.ts` 里那条 `toBe(expected)` 连 marked 结尾的换行都写死了，
服务器那份测试钉同一条输入的形状。这与两段 DDL、两侧语音核心、两侧访问日志是同一个模式：
**这个仓库里跨栈的重复是设计，不是疏忽，但它必须有一根能失败的线拉着。**

## 29. 画不出来的不是形状，是自由曲线：logo 六轮返工（2026-11-09）

`nest/client/public/logo.svg` 之前是占位（一个圆 + 一条抽象箭头）。换成真的红隼标记时，
**前四轮都失败在同一件事上**：用一条自由贝塞尔曲线去描「有机的头颈轮廓」。每一轮改完都在
自己没盯住的地方长出一个尖——

| 轮 | 想做什么 | 实际长出来 |
|---|---|---|
| 1 | 隼头侧影 | 太圆、喙太短，像雀 |
| 2 | 压平头顶 + 拉长喙 | 眼与颊纹连成一根棍，**看起来像第二只喙** |
| 3 | 把颊纹下移留间隙 | 喉部用直线闭合 → 下颈多出一根看得见的刺 |
| 4 | 喙改成直线出尖 | 喙收不回来，长成了一根针（蜂鸟） |
| 5 | 换成几何构造：正圆头 + 楔形喙 | 干净了；但颊纹压到喙的下缘，读成「喙基的黑线」 |
| 6 | 颊纹整条收回头圆内 + 喙缩短 | 通过 |

**结论不是「多改几轮」，而是换构造。** 圆 + 楔这种形状**没有可以出错的地方**：每个形状由
两三个参数决定，参数错了立刻看得见；而一条五段贝塞尔的头颈轮廓有十个控制点，任何一个偏了
都只会表现为「怪」，不表现为「错」。缩到 16 px（favicon）时更是如此——第五、六版的区别在
128 px 下才看得出来，在 16 px 下就是「糊」与「不糊」。

三条具体约束（每条对应上面一轮返工，都写进了文件注释）：**喙上缘必须是直线**（鼓包就成了桨）；
**颊纹必须是锥形且与眼留出可见间隙**（圆头粗线缩小后变成一个悬空的「!」，连着画就成第二只喙）；
**颊纹整条必须落在头的圆内**（压到喙的下缘就读成喙基黑线）。

两条工程侧的：

- **验证方法是「把真实文件挂到一个本地页面上看」，不是看代码**。`~/tmp/kestrel-logo/preview.html`
  用 `<img>` 直接引仓库里那个 SVG，一次排 16 / 32 / 56 / 128 四档 × 白底 / 深底 / 米色品牌底三种背景，
  在 Chrome 里截图看。这跟 §30 那次量 composer 几何是同一个路子：**不构建、不起服务**（硬规则），
  但用真实产物而不是复刻品。用完即删。
- **注释不要留在会发给用户的文件里**。第五版一度 2335 字节，其中大半是这套理由；而
  `logo.svg` 每个页面加载都要取一次（favicon + 顶栏）。理由挪到这里，文件里只留一行指针 +
  每形状一句约束，收到 **1118 字节**（占位版 294）。几何参数在两次改动之间完全没动过——
  我是靠比对 `shapes` 属性表确认「这次只是搬注释，没有改形状」的。

**没验的**：没有部署，所以线上取到的仍是旧占位；`make build-web` 之后它才进根 `public/`。
另外这是**手写的几何标记**，不是设计产出——它满足「认出红隼、16 px 不糊、三种背景都读得出」，
但品牌层面的判断仍然该由人做。

## 30. 「尺寸由模型选，暂无好办法」是没人去量过的结果（2026-11-09）

`roadmap.md` §3 里那条 Chrome *issue* 挂着「要修得先知道图片尺寸，而尺寸由模型选，暂无好办法」。
去量了一遍，两小时之内这件事就变成了两列 + 一个探针 + 四处模板。**这条记录的价值不在实现，
在那句"暂无办法"是怎么来的**：它是一个没被验证过的推断被当成了结论。尺寸其实一直在那里——
在落盘的字节的头 24 个字节里。

### 请求的尺寸不是图片的尺寸

最容易被当成显然正确的前提。生产 19 张图逐张对撞（`tool_calls` 里的 `size` vs 文件头）：
18 张一致，**1 张不一致——请求 `1024*448`，落盘 `1536×640`**。供应商把不支持的尺寸吸附到别的档位。
照请求值写进 `width`/`height`，得到的是一个**错宽高比的预留盒子**，比它要修的页面跳动更糟。

判据：**凡是"这个值大概是已知的"都要指出它具体存在哪一字节里**，说不出来源就当作不知道。

### 探针自己踩的三个坑

1. **统一的前置长度门槛会误杀合法文件。** 一开始写了 `if bytes.len() < 16 { return None }`，
   于是 GIF（头部只需 10 字节）永远读不出来——测试红了才发现。正确做法是每个格式自己用
   `get` 按边界取，截断自然返回 `None`，不需要一个早退的门槛。
2. **fixture 的 chunk 长度写错，测的就不是你以为的那条路。** RIFF 的 chunk 头是 4 字节 tag +
   **4 字节**小端长度。我第一版少写一个零字节、又声明了比实供字节更大的长度，于是探针走的是
   "文件被截断"分支——测试红了才暴露。判据：**手造二进制格式样本时，先算长度再写字节**，
   并且让"截断"与"格式不认识"各有自己的用例，否则一条断言会同时冒充两条。
3. **JPEG 的帧头是高度在前，PNG 是宽度在前。** 这两个顺序相反，是这类代码最常写错的一行，
   所以它在两侧实现里都有注释。

### 模板侧：handlebars 没有 `and`

想写 `{{#if (and this.width this.height)}}`，但这个应用只注册了一个 helper（`main.ts:40` 的 `eq`），
而**未注册的 helper 是渲染时抛错，不是编译期错误**——`tsc` 与单测之外没人拦得住，直到某个页面真的
渲染到那一行。改成嵌套 `{{#if}}`。顺带一条：这类"成对才输出"的规则，用两个 `if` 表达比引入一个
布尔 helper 更省事，也更不容易在下一个模板里被忘记注册。

### 旧行的路径格式没统一

对撞时按 `file_path` 直接找文件，只核到 13/19。差的那 6 条是**旧的绝对路径格式**
（`/app/storage/x.png`），也就是 architecture.md §5 说"仍被兼容"的那批。判据：
**拿资产行与磁盘文件对账必须按 basename**，读文件也一样——`resolveAssetPath()` 干的就是这件事，
别在测试或脚本里绕过它自己拼。

### 没加依赖这件事值得说

两个栈都没有图片解码库。加一个的代价分别是：Rust 侧编进发给每个用户的静态二进制
（§11.5 拒 `pulldown-cmark` 用的就是同一条理由），Nest 侧要动 `package.json` 与 lockfile。
而这里真正需要的只是四个魔数。**"要不要加依赖"要先问"需要的是库还是一个函数"**，
否则很容易为了读 8 个字节引入一个编解码器。

## 31. 修好了但 lint 还在报：量症状，别量代理指标（2026-11-09）

尺寸这件事按 §40 的测量做完、上线、回填，`assets` 19/19 都有尺寸，服务端发出的 HTML 里两张
`<img>` 都带着 `width="1024" height="1024"`——**然后 DevTools 那条
`Lazy-loaded images should have explicit dimensions` 仍然报 count=2，节点就是这两张。**

如果这时候还在用那条 issue 当判据，结论会是"修复无效"，而它是错的。改成直接量它代理的那个症状：

```js
// navigate_page 的 initScript，先于页面任何脚本
new PerformanceObserver((list) => {
  for (const e of list.getEntries()) if (!e.hadRecentInput) window.__ls.push(e);
}).observe({ type: 'layout-shift', buffered: true });
```

重载、再滚到底让懒加载的图真的进视口，读数：**`shiftsTotal=0`**，而且其中一张在
`complete=false / natural=0x0`（还没下载完）时**已经占着 758×758 的盒子**。
这条待办原本描述的现象——"图片突然展开把下面的正文推走"——前置条件已经不成立。

**为什么仍然报**：我第一个版本给的解释是「样式表 `.asset-image { width:100%; height:auto }` 把宽度
交给 CSS，属性只用来算宽高比，所以启发式不给分」。**这个解释当天就被自己的实验推翻了**：在
`/admin/conversations/3` 上注入一条更高特异性的规则 `img.asset-image{width:auto;height:auto}`
撤掉那个覆盖（`computedWidth` 从 758px 变成 1024px，证明注入生效），issue **照样报 count=11**。

所以现在只能报观测，不能报解释：计数等于该页懒加载图片数（2 张报 2、11 张报 11），
**与属性有无、与 CSS 是否覆盖都无关**。为什么查不到——本环境的搜索两次都返回空结果，
Chromium 那条启发式的确切条件没读到，所以任何"这样改就能消掉"都是瞎试；而它代理的症状已量到是 0。

**这条才是本节要留的东西**：一个未经实验的合理解释，写进文档之后就长得像结论。上一轮它被写进了
§42.3 与 roadmap §3，还带着"我没证实"的限定语——**限定语没能救它**，因为读者记住的是解释，
不是那个限定。规则：**解释要么配一个能推翻它的实验并当场跑，要么只写观测。**

**可迁移的那一条**：任何"浏览器报 issue/警告"类的待办，**先问清楚它是哪个症状的代理指标，然后去量
那个症状本身**。代理指标会因为它自己的启发式（这里还是未证实的那一条）给出与事实相反的结论；
而 `layout-shift` 这类是用户真实感受到的东西。反过来也成立：这条 issue 从此不能当作这个缺陷的判据，
`roadmap.md` §3 里已经按这个口径改写。

## 32. 打断里最容易想错的三件事（2026-11-09）

实现 §11.6 那条待办时，三个地方第一版都是错的。它们的共同点是：**看起来只是加一条帧**，
实际上动的是「谁拥有这一轮」的判断。

### 1. 新那句话的 `commit` 不能立刻发——这一条能静悄悄吃掉用户的话

直觉写法：按下 → 发 `cancel` → 开始采 → 松手 → 发 `commit`。**错**：中继那道闸
（§11.3 的 `Commit` 在轮次进行中被丢弃）不认「用户已经打断了」，它只认 `busy`。
所以那句盖上去的话会在松手那一刻被丢掉，**页面上一切正常**：状态写着「正在思考…」、
音频帧都发出去了、没有错误。这类形状我们已撞过一次（§8：漏发 `start` 帧，
控制台干净、页面健康、服务端在等一个永远不会来的 `ready`）。

落地的写法：`cancel` 之后**本轮还没结束**时，commit 存着，等页面收到那一轮的 `done`
再补发（`voice_core::close_turn` / `voice.ts` 的 `closeTurn`）。于是两条窗口都对：

- 上游有 response 在飞 → 中继当场收尾，`done` 很快来，那句盖话立刻提交；
- 中继没有 response 可取消（工具正在跑）→ 中继**不转发**（见第 3 条），`done` 在那一轮
  自然结束时来，盖话在那之后提交。这一条路径下打断只做到「静下来」，用户那句话排在
  本轮之后——不是丢，但也不是插队。

判据写成断言而不是一句注释：`an_interrupting_hold_hands_over_its_words_when_the_turn_closes`
与 Nest 那侧的「打断期间松手，话先存着，等本轮结束才 commit」。**如果没有这组断言，
第一版会带着一个静默丢句的 bug 上线**，而且线上看不出来。

### 2. 「按下即打断」是错的判据：必须是**已经听到声音**

页面知道「此刻喇叭在响」（`spoke`：本轮有过 `audio` 帧且本轮未结束），服务端知道
「上游此刻有没有 response」。这两个谓词不同源，但**同一个判据**才能既安静又不丢句：
只有 `spoke` 为真时打断，服务端必然也 `responding` 为真（音频只可能来自一个在飞的 response），
于是那条 `cancel` 永远有人接。反过来按「busy 就能打断」写，工具轮次里那一按会
把 `cancel` 送进一个没有推理可取消的上游——见第 3 条。

### 3. 上游对空取消回的是**错误帧**，所以 `responding` 必须自己数

官方那句是判据：「取消当前正在进行的推理……**若当前无进行中的推理，返回错误**」
（<https://help.aliyun.com/zh/model-studio/fun-audiochat-client-events>）。这条错误的形状
与「你刚才那句话被拒了」一模一样——它会变成页面上的一行红字，而用户什么都没做错。
所以中继侧 `responding` 是个自己维护的谓词：发 `response.create` 时置真，
`response.done` / `failed` 时置假，`cancel` 只在它为真时转发，否则只留一行 WARN 日志。

置真是**乐观**的（发请求那一刻，不等 `response.created`）。这个方向选错了会有代价，
但选错的那一侧代价更小：乐观 → 极窄窗口内可能白转一条 `cancel`（拿到一条错误帧）；
保守 → 用户按下时那句「刚开始还没出声」的回答永远停不掉。窗口是 `create` 与 `created`
之间，实测过的那条链路（§4）上它比人按下得快三个数量级。

### 4. 咽下那一帧 `response.done` 靠的是一个**上游承诺的顺序**

打断后中继已当场收尾，而它**欠**一帧 `status=cancelled` 的 `response.done`。这一帧必须被咽下
（`interrupted` 谓词），否则第二次 `done` 会把用户正按着的按钮「还」给他。
咽下不会误吞下一轮的 close，靠的是：事件在**这一条 socket 上按序到达**，
上游不可能在被取消的回答之前先开始下一次回答。这是文档级前提，不是我们量出来的读数
（§43.4 第 2 条）。因此还留了一条兜底：`ask_for_response()` 里把 `interrupted` 归假——
万一那一帧永不到，代价是那一轮余下的音频被丢，而不是之后的每一轮都被吞掉。

### 5. 两条静/动的分工：页面 `muted` + 中继 `interrupted`，都要

只在中继丢：`cancel` 上线那一刻已经离开服务端的音频帧仍会到页面，把刚停下来的声音接回去。
只在页面丢：那一轮余下的字节会继续往页面灌，占掉那条**每通话只有 64 帧**的出站队列
（~~65 KB/s 的上行~~ 这个说法当日即作废——它引用的是一条标错方向的读数，见 §34 与 verification.md §45）。
**同一件事的两端各留一份谓词**，
不是为了冗余，是因为它们各自漏掉的部分不同（在路上的 / 已排队的）。
页面上「已排队的」是靠把每个音频源记下来再 `disconnect()` 摘掉的——`web-sys` 的
`AudioBufferSourceNode::stop` 已废弃（挪到 `AudioScheduledSourceNode`），而我们的门要求
wasm 检查零 warning，所以用「从图上摘掉」而不是「停下来」：一个没有出边的源到不了任何人的喇叭。

### 6. 顺带量到的既有行为：松手之后麦克风并不关

`voice_core::audio_chunk` 只看 `blocked`，不看 `holding`；`voice.ts` 的 worklet 回调只看
`blocked` 与 `ready`。`stop_capture` 只在 socket 关闭与 `pagehide` 被调——**两套同形**。
于是第一次按住之后，环境声一直进上游缓冲，直到上游拒绝为止。

这条这次**没改**，两个理由：不在本次范围；而且它正是「打断时那句话已经在缓冲里」的
唯一来源（改了就得到「按下后必须等 `done` 再重新采一句」的另一种设计）。但它有可观察的
代价：两轮之间的环境声会进下一次提交，浏览器录音指示常亮。要不要收，是产品口径
（`roadmap.md` §3 已列）。**这是读码结论，不是运行读数**——真实麦克风那条至今未收口。

### 7. 一个流程上的自纠

本节实现过程中有两次 `apply_patch` 把**说明文字当代码**贴进了函数体（一次在
`voice_core.rs` 的 match 臂里，一次在测试模块里），都被编译器当场抓住。
规则：**hunk 里的每一行都必须是最终代码**，想留 TODO 就写 `todo!()` 以外的显式占位并当场编译。
穷尽 `match` 在这里是资产而不是负担——它同一轮还抓到了第三件事：新增 `ClientFrame::Cancel`
之后 `wire.rs::tag()` 立刻编译失败，而那条路径**没有任何运行时报错**（页面会静默地把
`cancel` 发成一个未定义标签，服务端把它当作不认识的帧丢掉）。

## 33. 「早退那一行把 `done` 也一起省掉了」——一个被打断放大的旧洞（2026-11-09）

写 §43 的时候需要确认一件事：页面那个「延后的 commit」是**等 `done` 落地**的，那 `done` 一定来吗？
回去读中继的收尾：

```rust
let (Some(message_id), Some(conversation_id)) = (self.assistant_message_id, self.conversation_id)
else { return; };          // <-- 连着这一行，`self.send(ServerFrame::Done)` 也不发了
```

`open_turn` 里那两次落库都是 `.ok()`——写失败不拦。于是有一条真实的链：
**占位行没写成 → 上游照样生成、照样回音频 → 这一轮收尾时因为没行可写而直接早退 → 链路上没有 `done`**。
在此之前它的后果是「按钮整通电话都禁用」。有了打断之后后果更重：页面停在静音窗口里，
而那句盖上去的话存成 `pending_commit`，**永远不会发**——用户说完话，页面不动、也不报错。

**为什么现在才暴露**：这个洞一直在，是打断**新增了一个依赖它的状态**（延后的 commit 只认 `done`）。
一般的那一条：**加一条依赖旧信号的新路径，会把旧洞从「不方便」升格成「丢数据」**——
所以新路径落地时，值得回头把它所等的那个信号的产生条件读一遍。

### 修法不是「无条件发」

第一版就是无条件发，然后一条既有断言当场红了（`a run of failures is one problem`）：
上游连回两次错误时，第二次 `Failed` 会再落一帧 `done`。所以判据收成一条谓词：

```rust
let closing = self.busy;          // 只有「确实有一轮在跑」才有权宣布它结束
self.busy = false;
```

这与那条既有规则同形：一轮的连续失败只往页面发**一行**错误。
`send()` 本身仍在 `closed` 时吞帧，所以 `close()` 里那次收尾不会把 `done` 发出去——
那条断言（`sent == ["close:1000"]`）早就钉着这件事，本次没动它。

### 同一个洞的邻支：`dispatch_pending` 那道早退

`response.done` 带着工具调用来的时候，如果 `conversation_id` / `assistant_message_id` 有一个是空，
原先是 `pending_calls.clear(); return;`——**调用丢了，轮次也永远不结**。这与上面那条是同一个形状：
「没有行可写」是我们的事，「这一轮结束了」是页面的事。所以那两行后面补上 `finalize_turn().await`
（它自己会走早退分支并发出 `done`）。两套同改。

这一条的**顺序是反的**：先修的、后写的断言。为了让它也有一个红灯，把那行 `finalize_turn().await`
临时撤掉跑了一次，`the turn still closes on the wire` 当场失败（`session.rs:936`），然后装回去重跑。
记在这里是因为：**「修完再补测」很容易变成「修完就不用测」**——补的那条断言如果不取一次红灯，
它就只是在描述我现在以为代码会做的事。

### 改了断言，而且改的是钉住洞的那一条

```rust
// 旧：assert!(frames(&mut out).is_empty(), "a run of failures is one problem");
// 新：错误帧 0 条，但刚提交过的那一轮必须留下一帧 done
```

原句「一个失败风暴是一个问题」说的是**错误行**，却被写成「整条链路上什么都不许有」——
顺手把该发的那帧也一起钉成了不许发。判据要指着那一类帧数，不能指着通道空不空。
这一条与 §31「量症状，别量代理指标」是同一类错的两种写法。

### 边界

- **没在活库上人为制造过一次写失败**，所以「页面卡住」这个现象只由代码路径与断言给出，没有截图。
- **Nest 那侧只改代码没加断言**：`nest/server/__tests__/voice/` 那个文件的既有边界是「中继层用假对象会失真，
  归脚本实测」（§4 那批），`VoiceConnection` 连导出都不是。测到的那侧是 Rust，两套同形由代码对照保证。
- **这是纯服务端修复**：两套客户端一行未动，`kestrel_client_bg.wasm` 的 md5 不变，
  所以下一次部署**在效果上**只需要换二进制。但别据此跳过构建：`make deploy` 的依赖是
  `build-linux build-wasm`，它照旧重打了包——15:02 那次跑完，主机上的包 md5 仍是
  `cf246955e7aa…`（与 §43.5 那份逐字节相同），这才把「客户端没动」从推断变成读数
  （`docs/verification.md` §44.4）。

## 34. 一个标错方向的数字：被引用进三处正文、两条代码注释，还撑起了一条决策（2026-11-09）

要办 roadmap §3 那条「并发通话数未评估」，第一件事就是拿宿主机的对外吞吐去除每通话的字节数。
手上那句是 deployment.md §1 的「服务器**出站**带宽约 **65 KB/s**，入站约 5 MB/s，相差 77 倍」。
按它算，一路通话（出声时下行 ~64 KB/s）就把整台机的对外带宽吃光了——结论会写成「并发上限 1」。

**这个结论是错的，因为那句读数本身方向写反了。** 括号里给的实验是「下载 8.7MB 的 apt 索引
60 秒未完成」，那测的是**服务器自己去公网拉**（数据流入服务器），不是它对外服务。今天分三条路径重测
（逐条测法与读数在 `docs/verification.md` §45）：

| 路径 | 今天的读数 | 与旧句的关系 |
|---|---|---|
| 服务器 → 浏览器（服务出网：wasm、图片、语音下行） | 冷连接单流 **1.9 MB/s**（380 KB / 0.193-0.201s，三次相差 <5%）；浏览器 h2 四路并行**聚合 ~5.9 MB/s** | 旧句把它叫「出站 65 KB/s」——**方向反了，而且按单流小 29 倍、按聚合小 ~90 倍** |
| 浏览器 → 服务器（麦克风上行） | §4.1 线上实拨：48 KB/s 实时推流 `bufferedAmount` 峰值 1305 字节，**无积压**（今天没重测） | 旧句把它叫「入站」，方向对，量级另一回事 |
| 服务器自己去公网拉（旧句实际测的那条） | Cloudflare 20 MB = **652 KB/s**；阿里云 CentOS 镜像 10 MB = **225 KB/s** | 比旧的 65 KB/s 快 3-10 倍；差异**没有解释**，只报读数 |

**这条是本节要留的东西**：一个测量数字被抄进文档时，如果只抄「多少」不抄「哪条路径、怎么测的」，
它就会在下一次被引用的时候变成一个方向相反的结论。这次它已经落了四处：deployment.md §1、
CLAUDE.md 的部署段、`.claude/skills/deploy/SKILL.md`，以及**我今天 14:2x 自己写进两套中继的两条
代码注释**（「keeps the bytes off the wire on a 65 KB/s uplink」）——那两条注释的结论（在中继侧丢掉
被打断那一轮余下的音频）是对的，理由是借来的，所以注释已改成引用**每通话 64 帧的出站队列**
这个代码里读得出的事实。

**顺带一条只报观测、不报解释的**：§39.1 今天 12:43 记的是同一个 wasm 走公网 **2.0 秒 / 376 KB**
（≈188 KB/s），与 15:36 的 **0.19 秒 / 380 KB**（≈1.9 MB/s）差 10 倍。两次都不是缓存
（`cache:no-store` + 唯一 query）。但**那次记录里的「带宽受限」这个解释自己站不住**：
同一批读数是 `/login` **3.2 秒**——那是一页 **909 字节**的动态页，3.2 秒对 909 字节不是带宽，
是握手或服务侧的等待；同批却又能 2 秒搬完 376 KB。所以 §39.1 当时看到的慢，原因**至今未定**，
只留这两条观测：**同一台机同一文件同一天两次相差 10 倍**，且**其中较小的那个不能用「出站带宽」解释**。
按 §31 的规矩：不写解释，也不许再拿其中任何一次当容量规划的唯一依据。

**可迁移的那一条**：凡是要用数字做判断（带宽、体积、延迟、行数），**引用它的人负责复测它**；
复测不了就在文档里写清「这个数字来自 X 测的 Y 路径，我没重测」。这一条与 §31
（量症状别量代理指标）、§30（没人去量过的「暂无好办法」）是同一类错的第三种长相：
**前两次是没量，这一次是量了但记错了方向。**

**这条数字撑起的不止是判断，还有一条决策。** 今天查同机容器，`justwork-authcenter-api` /
`-home-api` / `-admin-api` / `-recommend-api` 四个跑的都是 `golang:1.22` 基础镜像——
这台机**确实拉过**公共镜像并且一直在用。所以 deployment.md §1 那条「服务器侧零下载」原先是
**用一个错方向的读数（出站 65 KB/s）撑起了一条本来只需要「不需要」就能成立的决策**。
更正后的依据只剩一条：这个形态（`scratch` + 开发机上传静态二进制）根本不需要下载，
与外部条件允不允许无关。**教训的形式化说法**：写「因为条件 X 所以必须这么做」时，
先问一句「如果 X 不成立，这个做法还站不站得住」——站得住的话，X 就不该是它的理由。

被这句话污染的四处正文（deployment.md §1、CLAUDE.md 部署段、`.claude/skills/deploy/SKILL.md`
两处，以及我自己今天写进两套中继的两条代码注释）今天全部改了；
另有 §4.1 与 §39.1 两处**历史记录**引用过它——历史记录按惯例不重写，
只在原地补了当日更正（verification.md §39.1 末段与 §45）。

## 35. 关掉一个渲染器特性，能在哪一层关：marked 18 的四根钩子与一条错分类（2026-11-10）

要「让两套 markdown 同口径」，动作全在钩子选择上，而钩子选择**读文档读不出来**。§28 已经留了一条
（`tokenizer.html` 返回 `false` 是回落，真正生效的是 `renderer.html`）。这次把那一层机理一次看清了：

`Marked.use()` 对 `renderer` / `tokenizer` / `hooks` 三处的包装是同一句
`let c = mine.apply(...); if (c === false) c = defaultImpl(...); return c || ''`。
**只有 `false` 才触发回落**，于是内置特性在 tokenizer 层根本关不掉——你的 tokenizer 跑在内置之前，
返回 `false` 只是把控制权交回去。能关的层只有两处：

| 特性 | 能关的地方 | 踩过的坑 |
|---|---|---|
| 图片 | `renderer.image` | 别用 `token.href` 重拼源码。引用式图片 `![一张][ref]` 到这里时目标**已经解析过了**，重拼会得到 `![一张](https://…)`——一串谁都没写过的字。要用 `token.raw` |
| 任务列表勾选框 | `renderer.checkbox` | marked 造控件时把 `[ ]` 从条目正文里**剥掉了**（`listReplaceTask:/^\[[ xX]\] +/`），所以 marker 只能在这里放回去，放别处都会多一份 |
| 引用式链接 | `renderer.link`（判据是 `raw` 里有没有 `](`） | `tokenizers.linkReference`、`tokenizers.def` 两条**都试过、都无效** |
| `[label]: url` 那一行要留在页面上 | **只有 `hooks.processAllTokens`** | `def` 是一个没有 renderer 方法的 token，Parser 直接跳过它。所以在 renderer 层「把定义行渲染成文字」这件事没有对象可渲染，只能在 token 层把它换成一个段落 |

**第二条要留的是分类名，代价是哨兵数错了东西。** §36.2 与 §38.3 都把那一类叫「脚注」，
`scripts/markdown-census.sql` 因此只数 `%[^%`。但 **marked 18 没有脚注实现**（`grep footnote
lib/marked.esm.js` 命中 0 条），那条样本 `[^1]` + `[^1]: 说明一行` 走的其实是 CommonMark 的
**捷径引用链接**——所以真名是「引用式链接」，而 `[文档][ref]` 这种不带 `^` 的写法原先一条都数不到。
现在 census 加了 `引用式链接 ][（退回文字）` 那一行。**形式化的说法**：给一类缺陷起名时用了
*上层意图*的名字（脚注）而不是*实际机制*的名字（引用链接），下游所有按名字写的探针都会漏。

**第三条不属于 marked，属于任何手写的行内/块级两遍**：`~~~js` 在旧的 `crates/md` 里被渲染成
`<del>~js console.log(1)</del>`。原因是块级那遍只认 ```` ``` ````，这一行于是落进段落，而行内那遍
有 `~~` 配对。**块级识别少一条语法时，内容不会停在「不渲染」，它会掉进行内那套完全不同的语义里。**
凡是「子集 + 手写两遍」的渲染器都要守这条顺序：加语法时先补块级分支，再谈行内，且每个块级分支
都要有一条样本钉住它没有被行内那遍二次解释（本次是 `assert_eq!(markdown("~~旧值~~"), "<p><del>旧值</del></p>")`
与 `markdown("~~~js\n…")` 这一对，两个方向都钉）。逐条读数在 verification.md §46。

**第四条：把递归渲染器放进单进程服务之前，先问「崩的是请求还是进程」。** 收了引用块、嵌套列表、
条目内部的块之后，`crates/md` 从「逐行扫」变成了真递归，而它的输入是**模型写的**正文（可被提示注入）。
两边同一份输入的崩点（一行 `>` × N）：Rust 在 5,000 层 `has overflowed its stack` → **进程终止**，
这台机是单进程 SSR + 所有在连的 SSE，于是一次注入带走所有人的这一轮；`marked` 在 5,000 层抛
`RangeError`（3,000 层还能过），那只是一个请求失败。**崩的粒度不同，所以两条防护措施也不同**：
Rust 侧是一条 `MAX_NESTING = 32` 的硬上限（到顶按文字显示，剩余工作因此是线性的），Nest 侧是出口那个
try/catch（抛出退成整段转义文字）。定 32 的依据不是「够用」这种说法，而是两个崩点之间的距离与
线上深度（§46.5：引用块 0 条）。**可迁移的那一条**：给「深度由输入决定」的递归加保护时，
先量出两边的崩点，再决定是**收上限**（进程级失败）还是**接异常**（请求级失败）——反过来做会写出
一个「捕获了异常但进程照样被栈溢出带走」的假防护。

**第五条不是发现而是方法论，代价是六轮。** §38.3 那句「一致 11 / 分歧 8」用的是 19 条样本，
按同一套做法一路加到 80 条，**每一轮都量出上一轮不可能看见的分歧**（§46.2 那张表），
而且新分歧全是上一轮那条规则动出来的边角：收了 `1)` 就要问 `+`，收了嵌套就要问松散列表，
收了「缩进不是代码块」就要问缩进的围栏。**「把已知的都关了」和「已知的是完整的」是两句话**——
这次如果不是把样本面一轮轮 widen，写进文档的会是「8 类已关」，而实际还漏着 13 类。
判据也因此换了形状：不是「两套一致」，而是**同一条输入在三处钉同一段输出**，
外加一句 `structurally_identical=80` 说的是这 80 条，不是说构造空间穷尽了。
