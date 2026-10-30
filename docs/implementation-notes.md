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
