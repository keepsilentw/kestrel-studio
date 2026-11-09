# kestrel-studio 部署与运维

> 本文只覆盖**服务器侧**：构建形态、端口、反代、域名与运维命令。
> 本地搭建期的环境坑见 [implementation-notes.md](implementation-notes.md)。
> Claude 面向的操作手册见 `.claude/skills/deploy/SKILL.md`（含排障流程与更多约束说明）。

## 1. 部署形态：服务器端零下载构建

**2026-11-09 更正（读数标错了方向）**：本节原先写「服务器**出站**带宽只有约 65 KB/s（实测：
下载 8.7MB 的 apt 索引 60 秒未完成），而入站约 5 MB/s——相差 77 倍」。那个实验测的是
服务器**自己去公网下载**（入流），不是它对外服务；方向写反了，而且今天的复测连「下载慢」这半
也只剩 3-10 倍而不是 77 倍。今天同一台机的三条读数（逐条测法见 verification.md §45）：

| 哪条路径 | 今天的读数 |
|---|---|
| 服务器 → 浏览器（服务出网：wasm 包、图片、语音下行音频） | 冷连接单流 **1.9 MB/s**（380 KB / 0.193-0.201s，三次相差 <5%）；浏览器 h2 四路并行**聚合 ~5.9 MB/s** |
| 浏览器 → 服务器（麦克风上行） | 48 KB/s 实时推流**无积压**（§4.1 线上实拨的读数，今天没重测） |
| 服务器自己去公网下载（原句实际测的那条） | Cloudflare 20 MB 跑完用 30.7 秒 = **652 kB/s**；阿里云 CentOS 镜像 45 秒到时限被截断、当时已拉 10.1 MB = **225 kB/s**（curl 千进制） |

**决策不变，理由换一条**：仍然「任何需要服务器下载的方案都不做」——`apt install` 工具链、
`pnpm install`、本机建 VM 交叉构建，一律不做。但成立的理由不再是「77 倍不对称」，而是这三条：
按今天那条下载速率折算（不是实测某次拉取）：一个 180 MB 级的基础镜像要 **4.6-13 分钟**，
且完全受对端限流摆布；宿主机是 CentOS 7
（glibc 2.17），装编译工具链本身就是一条坑；而 `scratch` + 开发机上传静态二进制这条路
**根本不需要下载**。

**这条决策是本项目范围的，不是宿主机性质的反证**：2026-11-09 查同机容器，另外四个生产容器
（`justwork-authcenter-api` / `-home-api` / `-admin-api` / `-recommend-api`）跑的都是
`golang:1.22` 基础镜像——也就是这台机**拉过**公共镜像并且用得好好的。所以「拉不动」不是这条规则的依据，
依据只有一条：**我们这个形态不需要它**。慢的下载路径（225-652 KB/s）只说明「别把可选的下载变成必需」。

**引用一个测量数字时必须连着它的方向与测法一起抄**——这次它被引用进了两处代码注释、
两份文档和一份操作手册（implementation-notes.md §34）。

最终方案：镜像在服务器上**组装**，但服务器不下载任何东西。

**2026-10-30 起这条管线换成 Rust**（roadmap.md §4 的 P2），形态比 Node 时代更简单：

| 需要的东西 | 来源 |
|---|---|
| 服务端可执行文件 | 开发机交叉编译的 **x86_64-unknown-linux-musl 静态二进制**（`make build-linux`），随构建上下文上传 |
| 运行时基础镜像 | `scratch`——Docker 对它有特殊处理、**永远不需要拉取**，所以服务器上没有任何要缓存的镜像 |
| 前端资源 | 开发机构建的 `public/`，两份客户端产物都在里面：`assets/*.js`（Nest/Vite）与 `wasm/*`（`make build-wasm`）。页面加载哪一份由 `CLIENT_BUNDLE` 决定，默认 `nest`，见 §2.2 |
| TLS 根证书 | 编进二进制的 `webpki-roots`（scratch 里没有 `/etc/ssl/certs`） |
| `/tmp` | compose 的 `tmpfs`：scratch 没有 `/tmp`，而 SQLite 需要临时 b-tree 时会去那里写文件 |
| C 交叉编译工具链 | 开发机上的 `zig`（自带 musl sysroot）；`ring` 的 C/asm 与 `libsqlite3-sys` 的 bundled SQLite 都靠它 |

静态链接是这里的承重选择：宿主机是 CentOS 7（glibc 2.17），动态链接的二进制在宿主机上
会撞 `GLIBC_2.27 not found`；musl 静态二进制连 libc 都不需要，`scratch` 就能跑。上传体积
从 node_modules 的约 300 MB 降到几 MB，服务器侧不再有编译步骤（原来那一步是用 golang 镜像
里的工具链重编 better-sqlite3，因为上传的 node_modules 来自 macOS，二进制是 darwin-arm64）。

交叉编译靠的是 `cargo zigbuild` + `zig`（zig 自带 musl sysroot，所以 `ring` 的 C/asm 与
`libsqlite3-sys` 打包的 SQLite 都不需要另装交叉 gcc）。三个前置件缺一个就停：`zig`、
`cargo-zigbuild`、`rustup target add x86_64-unknown-linux-musl`——`make build-linux` 会逐个
检查并给出装法。产物落到仓库外（`/tmp/kestrel-studio-linux/kestrel-studio`），并要求
`file -b` 显示 `statically linked`。**曾经试过手写垫片**（`scripts/cross/` 下四个转名脚本）：
C 侧编得过、链接过不去，2026-10-30 当天删掉换成 zigbuild，全过程与根因见
[implementation-notes.md](implementation-notes.md) §14——`scripts/cross/` 现在**不存在**，
看到旧文档提到它不要再去找。

**2026-11-02 13:24 起线上就是这套形态**（当天三次部署，在跑的是 13:47 那版）：镜像 10.2 MB
（Nest 时代 1.27 GB）、静态 ELF 直接跑、`docker stats` RSS 1.2 MiB，`/login` / 静态资源 /
鉴权跳转 / 公网反代 / 一轮真实流式对话均已确认，证据见 [verification.md](verification.md) §13。

### 1.1 把 Nest 管线重新上线（原「回滚到 Node 管线」）

**2026-11-07 起这一节不是回滚手册**：roadmap.md §4.5 撤销了退役，`nest/` 与 `rust/` 两套后端
长期并存，所以下面这套步骤的意义是**让 Nest 作为第二后端重新对外服务**，而不是认错退回。

**入口形态（2026-11-07 定，2026-11-09 撤销）**：原定「两个子域各反代到一套后端」，
**现在没有第二个子域，也不新设**——所以 Nest 那套**不对外服务**。下面这段原因留着，因为它是
下次再有人动这个主意时的第一手依据：两套写的是**同名同作用域**的 cookie——两边都叫 `connect.sid`、
都是 `Path=/`（`nest/server/src/main.ts` 的 session 配置与 `rust/server/src/auth/session.rs:12,58`），
同一个 origin 下按路径分流会让两套互相覆盖对方的 cookie 值，而签名口径又不同（§1.2），结果是每次
跨路径浏览都被登出。而**端口帮不上忙**：cookie 的归属只看 host 与 path，端口不参与
（RFC 6265 的比较算法里没有端口这一项；本部署没在浏览器上实测过，见 verification.md §37.3 末），
所以 `host:8848` 与 `host:8849` 共用同一个 cookie 槽位。**剩下的唯一隔离维度是另一个 host**，
而没有它——结论就是第二套只能不对外。

**2026-11-07 的落地形态**：Nest 有了自己的 `Dockerfile.nest` 与自己的 compose service，不再需要
「把三个文件退回切换前的提交」那套动作。

```bash
make deploy-nest   # 打包 dist + node_modules + public（归档约 96 MB）→ 上传 → 宿主机建镜像；不启动
make nest-up       # 真的把第二后端起来 —— 先读下面那条闸
make nest-down     # 停掉它，Rust 那边一动不动
make nest-logs
```

- compose 的 `nest` service 挂在 **profile** 上（`profiles: ["nest"]`），所以 `make deploy` 与
  `docker compose up -d` 这些既有路径**一个字都没变**：第二后端不会因为一次普通部署被顺带拉起来。
  `make deploy-status` 因此改成 `docker compose ps --all`，否则第二后端在核对里是隐形的。
- 镜像是**单阶段**的，基础层只用宿主机已缓存的 `dockerproxy.net/library/node:24-bookworm-slim`
  （建完 534 MB）。旧管线里那个 golang:1.24 的原生编译阶段不需要了：better-sqlite3 v13 在包里带了
  八个平台的 prebuild，require 时按平台挑。实测那条 `node-gyp rebuild` 只产出一个空的
  `build/Release/obj.target`，而应用照样开库、`/login` 200（verification.md §28.1，
  踩坑台账 implementation-notes.md §22）。宿主机上那六层 1.3 GB 的 `<none>` 是旧管线的遗产，
  与这条路径无关了。
- **`NEST_BASE_IMAGES=1 scripts/bootstrap-host.sh` 这一步也不再需要**（node 基础层已在）。全新宿主机
  才按 §1 的镜像站规矩预热。
- 盘上 2026-09-27 那份残留（`src/`、`dist/`、拆分前的根 `node_modules`、`nest-cli.json` 等）跟这条
  路径再无关系——`deploy-nest.sh` 每次按当前 monorepo 布局重新打包上传。它们现在只是没清理的磁盘占用。
- 端口按 §2 那条老规矩由 compose 注入（`PORT=${NEST_HOST_PORT:-8849}`），**不进那份永不覆盖的
  `.env`**：`.env` 里还留着 `PORT=8848`，两套各自的 `environment` 会盖住它，谁也不会读到那个残留值。

两套**同时在线**时是两个 service、两个宿主端口（Rust 占 8848，Nest 默认 8849），都只绑在宿主机上，
对外只有 Rust 一个入口。**原先那两件随之而来的配置项已作废（2026-11-09）**：

1. ~~openresty 要新增一个 server 块（`/opt/1panel/www/conf.d/<新子域>.conf`）~~ —— 没有第二个域名，
   所以不做。§3 那三个关键点（SSE 不缓冲、语音的 `Upgrade`/`Connection`、以及 `proxy_buffering` 的
   坑）仍然是真的，只是只对现有这一块生效。
2. ~~每套自己的 `PUBLIC_BASE_URL`~~ —— 同理不做。留着这条的原因是它记着一个**没实测过**的事实：i2v
   是把签名帧 URL 交给上游让它自己来取的，而两套的 `SESSION_SECRET` 相同（同一份 `.env`），签名能不能
   被另一套验通谁也没验过——别把「凑巧能用」当成设计。第二套哪天真要对外，这个未验证点会一起回来。

**`make nest-up` 前面那道闸已经拆了（2026-11-09）**：architecture.md §13.2 那三处没有所有权的写
（启动收尾抓走对方的轮次、任务 worker 不 claim、`seq` 没有单一写入者）两侧同改到位（单元层读数
verification.md §31），**并且两套同时跑同一份 SQLite 文件已在库副本上实测通过**（§34：第二次启动的
迁移幂等、并发写下每会话 `seq` 仍连续且全库零重复、对方真实在飞的轮次不被收尾、新鲜外来租约跳过 /
过期租约接管）。「会不会坏数据」这一问已经有了答案。而入口那件事现在也**不存在**（上面那条决定），
所以 `make nest-up` 的意义只剩「在宿主机内部把第二套起来跑一跑」：它不接真实用户流量，起来之前请
仍然按本节末那条用库副本，别让它空挂在 `./data` 上——不是因为它会坏数据，而是因为没有理由这么挂。

**下次要复现这套冒烟环境照 §34.1 那三条命令**（要点：`sqlite3 .backup` 可以在生产容器正跑的时候取一致
副本；两套都绑 `127.0.0.1` 的独立端口、共享同一份副本与同一份 `.env`；跑完 `docker rm -f` +
`rm -rf` 副本目录，别把副本留在 `/tmp` 上过夜）。

**顺序有讲究**：那三列与那个 UNIQUE 索引是在**进程启动时**建的，所以先 `make deploy`（Rust 那侧跑迁移，
它同时是主交付链），再 `make nest-up`。反过来的话，先起来的 Nest 会建好同样的列——两侧 DDL 同形，不会
坏，但 Nest 会在 Rust 还带着旧代码（`MAX`+`INSERT` 两步、无 owner）时就并行，那个窗口的形状只写在
verification.md §31.4，没实测过。

**这一序已经走过一次**：2026-11-09 09:49 的 `make deploy` 就是「Rust 先迁移活库」那一步的落地——
预检（停容器 → 快照 → 本地只读）查出 0 对重复游标，所以那次只补了三列并把索引换成 UNIQUE，没有重编号
任何行；回查读数在 verification.md §32.2-§32.3。**下一步是并行实测**（两套同时挂同一个 `./data`），
过了才有 `make nest-up` 接活库。

**Nest 重新接管流量时，会再登出一次所有人**：Nest 只认 `sess.passport.user`，读不懂 Rust 写在顶层 `user` 的行
（反向是通的，Rust 两个位置都认）。所以凡是在 Rust 上登录过的会话，退回 Nest 后都要重新登录。
机制与实测见 [verification.md](verification.md) §13.4 与 architecture.md §13.2。

### 1.2 切换的实际后果（2026-11-02 已发生一次）

Rust 与 Nest 的 session cookie **签名口径不同**，任何一侧接管网上的流量都会让已有登录态失效，
表现为所有人重新登录一次。差别不在算法——两边都是 `HMAC-SHA256` + base64url-no-pad，
差在**被签的字符串**：express-session（`cookie-signature`）签不含 `s:` 前缀的 token，Rust 签
`"s:" + token`，所以同一个 token 算出的摘要必然不同。加上 §1.1 说的 sess 行形状只有单向读得通，
结论是**两个方向的切换都会登出所有人**。这不是数据损坏，但**必须提前告知使用者**，不能当成意外。

**2026-11-07 补一句（roadmap.md §4.5：双后端长期并存）**：这条性质从「切换日的一次性代价」变成
**常态约束**——同一个浏览器的登录态只在签发它的那一套里有效，所以两套后端**不能按请求轮询分流**，
只能按入口固定（一个人恒定落到同一套）。要同一账号在两套之间自由落，得先把 cookie 签名口径与
`sess` 行形状对齐，那是一次代码改动，不在本节范围内。

### 相关约束

- **Node 不能直接装在宿主机上。** 服务器是 CentOS 7（glibc 2.17），
  Nest 12 / Vite 8 需要 Node 20+（glibc 2.28+）；实测官方 linux-x64 二进制报
  `GLIBC_2.27 not found`。这也是 pm2 方案不可行的原因。Docker 是唯一可行运行时。
- **基础镜像来自镜像站。** 服务器上 Docker Hub 不可达，用 `dockerproxy.net`
  （实测 `hub.rat.dev` 超时、`docker.nju.edu.cn` 直接失败、`docker.m.daocloud.io` 与
  `docker.1ms.run` 卡住）。这条约束**目前只剩重新上线 Nest 时用得上**：现行 Rust 管线的运行时基础是
  `scratch`，Docker 从不拉取它，新宿主机不需要缓存任何镜像。
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

## 2.1 访问日志开关（`RUST_LOG`，2026-11-02）

`docker-compose.yml` 的 `environment` 里有一条 `RUST_LOG=${RUST_LOG:-info}`——按上面那条教训放在
compose 而不是 `.env`，理由一模一样：`.env` 永不覆盖，一次临时 `debug` 留在里面就会变成永久的
冗长日志。

默认 `info` 下**每请求零输出**（Rust 侧的 `TraceLayer` 全部打在 DEBUG，见 architecture.md §13.3）。
要取证就在宿主机上单独起一次：

```bash
ssh lavo-test "cd /opt/kestrel-studio && RUST_LOG=debug docker compose up -d"
# 走完一遍之后取日志
ssh lavo-test "docker logs --since 10m kestrel-studio 2>&1 | grep -E 'method=GET|status=' | tail -40"
# 收尾：回到默认（make deploy 不转发 RUST_LOG，下一次部署自然就把它清掉）
ssh lavo-test "cd /opt/kestrel-studio && docker compose up -d"
```

四条读法上的限制，别指望它超出自己能证的范围：

1. **只有旧日志要去色**：2026-11-02 17:29 之前写进日志文件的行带 ANSI（字段名与 `=` 之间夹着
   `\e[0m`，`grep 'method='` 因此 0 命中，第一次取证就被这坑到过），那部分要先
   `sed 's/\x1b\[[0-9;]*m//g'`。此后容器输出是纯文本（`main.rs` 按 stderr 是否 TTY 决定），
   上面的 grep 直接命中。根因、测试与线上复测见 verification.md §13.8。

2. **`/api/voice` 不在日志里**——那条升级路由刻意排在 `.layer()` 之后（architecture.md §13.3）。
   语音的真实证据是 INFO 上的 `voice session opened`，不需要这个开关。
3. **流式响应的 `latency` 是首字节时间**，不是一轮或一次通话的时长。
4. DEBUG 里除了访问日志还有 sqlx 的语句 span，量大；只在走查的那几分钟开着，走完关掉。
   **2026-11-06 实测这条被违反了**：11-03 13:23 那次拨档把 `RUST_LOG=debug` 带进去就没收回，
   三天后日志文件 16 MB、31,840 行里 97% 是 sqlx 的语句行（task worker 与会话清扫各每 8s 一次）。
   json-file 驱动且 `Config={}`——**没有 max-size，也没有 max-file**，所以它只会一直长。宿主机
   `/` 当时还有 50 G 可用，不急，但这与「可变项由 compose 注入、收尾就退回默认」那条规矩相冲。
   收回而保留灰度：`CLIENT_BUNDLE=wasm docker compose up -d`；两者一起退回：`docker compose up -d`。
   逐条数字见 [verification.md](verification.md) §21。
   **2026-11-06 13:58 已收回**：那次重拨灰度时故意不带 `RUST_LOG`，档位回到默认 `info`——新容器的日志
   文件 4.0 KB，五分钟窗口内 DEBUG 行计数 0。**同日 16:44 根因也关了**：`docker-compose.yml` 的
   `logging` 段加了 `max-size: 10m` / `max-file: 5`——在此之前 `LogConfig.Config` 是 `{}`，既没有大小
   上限也没有轮转，所以一次忘记收回的 `debug` 就会一直长（§21 那三天涨到 16 MB）。`10m × 5` 是给
   冗长档位准备的天花板，不是目标：`info` 档下一天只有几 KB。**注意这条上限只在 `up -d` 重建容器时
   生效**，`docker compose restart` 不改配置；生效核对方式是
   `docker inspect kestrel-studio --format '{{json .HostConfig.LogConfig}}'`（实测见
   [verification.md](verification.md) §24.1）。另外提醒一句：加的是日志上限，不是把 `RUST_LOG`
   塞进那份"永不覆盖"的 `.env`——那条规矩见 §5 与本节开头。

## 2.2 浏览器包切换（`CLIENT_BUNDLE`，2026-11-03）

`docker-compose.yml` 里 `CLIENT_BUNDLE=${CLIENT_BUNDLE:-nest}`，和 `PORT`、`RUST_LOG` 同一条规则：
它是部署配置而不是密钥，所以不进那份"只写一次、永不覆盖"的 `.env`。

| 值 | `/` 与 `/voice` 加载 | 用途 |
|---|---|---|
| `nest`（compose 默认） | `assets/main.js` / `assets/voice.js` | 代码默认与 `make deploy` 的默认，也就是**回滚档**：去掉环境变量就回到这份 |
| `wasm` | 内联 module loader → `/wasm/kestrel_client.js` + `_bg.wasm`，再调 `initChat` / `initVoice` | **当前线上档位**（11-03 13:23 拨上，之后每次部署都手工重拨；11-07 16:28 之后是 11-09 09:49 / 11:37 / 12:43 / 13:51 / **14:45**，最近一次的核对见 verification.md §43.5）——roadmap.md §4.3 P3 的灰度 |

```bash
# 打开：只重启容器，不重新构建、不重新上传
ssh lavo-test "cd /opt/kestrel-studio && CLIENT_BUNDLE=wasm docker compose up -d"
# 关掉（回到默认）：同一条命令去掉变量
ssh lavo-test "cd /opt/kestrel-studio && docker compose up -d"
```

四个必须知道的性质：

1. **它决定服务端会登出人吗？不决定。** 换的是页面加载哪个脚本，session 与 cookie 一行不动。
2. **`make deploy` 会把它带回 `nest`**：脚本不转发这个变量，所以下一次部署自动回滚。想让它长期
   保持 `wasm`，就把它写进服务器上那份 `.env`（`deploy.sh` 永不覆盖该文件，改它本来就是运维的活）——
   这是**长期启用**的唯一写法，别以为 `docker compose up -d` 的效果能扛过一次部署。
3. **启动日志会写用了哪个**：`client=nest` / `client=wasm`（`main.rs`）。判断"用户拿到的是哪份
   客户端"不必猜，也不必读宿主机环境变量。
4. **它按 `nest` 起、按 `wasm` 灰度时，出错是页面级而不是服务级**：接口、SSR、登录都还在跑同一份
   Rust 服务端。WASM 加载失败时 loader 会在页面上落一条 `.client-error` 提示并 `console.error`；
   回滚就是上面那条命令。

## 2.3 Nest 侧的访问日志开关（`ACCESS_LOG`，2026-11-07）

§2.1 那个开关是 Rust 二进制的（`RUST_LOG`）。Nest 那套现在也有同形状的访问日志了——
`nest/server/src/common/access-log.ts`，是 roadmap.md §4.5 撤销退役之后补上的（原来那句「刻意不同步」
的理由就是「反正要删」）。开关是 `ACCESS_LOG=1`（`true` 也认，其余一律关），放在 compose 的
`environment` 而不是那份永不覆盖的 `.env`——与 §2 同一条规矩。

```bash
# 打开（要等 Nest 这个 service 重新上线，service 名以 §1.1 落地时定的为准）
ssh lavo-test "cd /opt/kestrel-studio && ACCESS_LOG=1 docker compose up -d"
```

三处与 Rust 侧的口径差异写在 architecture.md §13.3，最要紧两条：**它只放开访问日志**，不会像
`RUST_LOG=debug` 那样把 sqlx 的语句一起放出来（§2.1 第 4 条那次 16 MB 的事故在这里没有对应物）；
`latency` 记到响应结束，所以两条流式路由上它是整条流的时长，**不能与 Rust 的首字节时间直接比**。

这一节是**实现的记录，不是取证的记录**：compose 里目前只有 Rust 一个 service，Nest 那套还没重新
上线（§1.1），所以上面这条命令没有在宿主机上跑过。单元层的形状与结果见
[verification.md](verification.md) §27。

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

**域名上是有人扫的**（2026-11-06 从应用访问日志里读到，明细见 verification.md §21）：11-04 与 11-05
两天 116 行请求，绝大多数是 `/.env`、`/web/.env`、`/wp-config.php.bak`、`/storage/logs/laravel.log`
这类 PHP/Laravel 探针，外加 11-04 01:57 一秒内的四个 POST（三个 `/login`、一个打在 `/` 上）。
因为 80/443 是唯一放行面，这些请求全部落到本应用的 fallback 上。实测响应：`/.env` 404、
`/../.env` 400、`/assets/../data/kestrel-studio.db` 400——静态根只有 `public/`，
`http/static_files.rs` 的 `resolve_under` 拒绝 `..` 与 NUL，所以服务器上那份 `.env` 与库文件
不在这个面的可达范围内。唯一还能取到的点文件是 `/wasm/.gitignore`（构建产物自带的 1 字节忽略文件）。
两个服务端都没有登录限流或失败锁定；这台机器上跑的是内部试用域名，要不要加是个排期决定，
不是这条记录要替谁做的决定。

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
| `make deploy` | 完整重新部署：先 `build-linux` + `build-wasm`，再跑 `scripts/deploy.sh` |
| `scripts/bootstrap-host.sh` | 每台新宿主机执行一次，检查 Docker 可用；只有回滚 Node 管线时才需要它拉两个基础镜像（`NEST_BASE_IMAGES=1`） |
| `make deploy-status` | 查看容器状态 |
| `make deploy-logs` | 跟随容器日志 |
| `make deploy-restart` | 重启容器 |
| `make deploy-down` | 停止并移除容器（保留 `data/`、`storage/`） |
| `RUST_LOG=debug docker compose up -d`（宿主机上） | 临时打开每请求访问日志，取证用；见 §2.1 |

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
   读取时按运行中的 `STORAGE_DIR` 解析（`nest/server/src/media/asset-path.ts`）；旧行里的绝对路径仍然认，
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

**2026-09-27 补上的一项**：超管登录线上实例并浏览管理页已走通——`/admin` 账号页、
`/admin/conversations` 列出 6 个会话、`/admin/conversations/1` 回放 2 个 turn 且
**无输入框、不加载 `/assets/main.js`**（只读回放的判定条件全中）。同一批操作里完成凭据
轮换：旧的内置口令与 `admin` 的默认口令均已失效，实测 `/login` 对它们返回
`/login?error=1`。新口令只落在服务器的 `.env` 与管理页里，仓库与文档不留凭据。
