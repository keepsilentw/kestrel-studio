# kestrel-studio 实测与验证记录

> 带日期的证据台账。**结论会过时**——端点行为、模型行为都可能变，引用前先确认日期与适用范围。
> 设计决策本身见 [architecture.md](./architecture.md)。
>
> **路径口径（2026-11-02 目录拆分）**：本文是带日期的事实记录，条目里的 `src/`、`__tests__/`、
> `client/`、`proto/`、`md/` 与 `cargo test --manifest-path client/Cargo.toml` 这类命令都按
> 当时的目录书写，现分别读作 `server/src/`、`server/__tests__/` 与 `web/__tests__/`、
> `web/client/`、`crates/proto/`、`crates/md/`（命令加 `--manifest-path web/client/Cargo.toml`）。
> 现行结构见 [architecture.md](./architecture.md) §9。**记录的数字与结论本身未经重测**，
> 目录改动只挪文件，不构成新的验证。

## 1. 前提实测结论（2026-09-20）

动手实现前对百炼 Token Plan 端点的行为摸查，用于决定是否引入 `openai` SDK。

| 项 | 结果 |
|---|---|
| `/compatible-mode/v1/responses` 支持 `stream: true` | 是 |
| reasoning 事件 | `response.reasoning_text.delta`，实测单次 118 个 delta |
| 正文事件 | `response.output_text.delta` |
| function calling | 支持。`response.output_item.added` 携带 `type: "function_call"`、`call_id`、`name`，参数以 `response.function_call_arguments.delta` 增量给出 |
| 端点非标准行为 | 每帧附带 `:HTTP_STATUS/200` 注释行与 `id:N` 行，手写解析需容忍 |

实测事件序列（纯文本请求）：

```
response.created
response.in_progress
response.output_item.added            (reasoning item)
response.reasoning_text.delta    x N
response.reasoning_text.done
response.output_item.done
response.output_item.added            (message item)
response.content_part.added
response.output_text.delta       x N
response.output_text.done
response.content_part.done
response.output_item.done
response.completed
```

带工具时的额外序列：

```
response.output_item.added            (function_call item，含 call_id / name)
response.function_call_arguments.delta   x N
response.output_item.done
```

**结论：手写 fetch + SSE 解析，不引入 `openai` SDK。** 事件模型已完全摸清，
端点还带非标准注释行，引入 SDK 只增加不确定性。

### 当时列出、现已验证的两项

原文列出「仍待验证」两条，其后均被 §2 的端到端跑通覆盖：

| 待验证项 | 现状 |
|---|---|
| 工具结果的回灌格式（`function_call_output`）与多轮续跑 | 已验证：§2 的实测中 `tool_call` / `tool_result` 各 1 次，回灌后续跑正常 |
| 文生图模型在该端点上的复验 | 已验证：§2 产出 PNG 2,009,073 bytes。注意文生图走的是**另一条路径** `/api/v1/services/aigc/multimodal-generation/generation`，与 chat 的 `/compatible-mode/v1/responses` 不同 |

## 2. 端到端验证结果（2026-09-20）

一次真实对话（prompt「画一只站在岩石上的红隼，清晨逆光」）的实测数据：

| 项 | 结果 |
|---|---|
| 登录（错误密码） | 302 → `/login?error=1` |
| 登录（正确密码） | 302 → `/`，Set-Cookie 正常 |
| 未登录访问 `/` | 302 → `/login` |
| 未登录访问 `/api/conversations` | 401 |
| `POST /api/chat` | 200 `text/event-stream` |
| 首字节 reasoning | 987ms |
| 整轮耗时 | 20.8s（含一次文生图） |
| 事件计数 | connected 1 / reasoning 3 / text 60 / tool_call 1 / asset 1 / tool_result 1 / done 1 |
| 模型行为 | 自动把中文口语 prompt 扩写为专业绘图指令，并带上 `size` 参数 |
| 产出图片 | PNG，2,009,073 bytes |
| 图片下载端点 | 200 `image/png` |
| 持久化 | 2 条消息；助手消息 reasoning 与 toolCalls 均已落库 |
| SSR 历史回放 | 思考过程块、图片、下载链接、会话高亮均正确渲染，无未渲染的 hbs 占位 |

### 覆盖范围说明

本表覆盖**后端链路**，本地与线上均通过。浏览器控制台当时因 chrome-devtools MCP 与 Chrome 的
连接问题未查，后来补查：对话页零错误零警告，另有 1 条既有的 Chrome *issue*
（`loading="lazy"` 的资产 `<img>` 缺尺寸），记在 [roadmap.md](roadmap.md) §3；
管理页的复查见本文 §5.2。

日后改用其他模型时需重新确认：该模型在 Responses API 上是否仍以 `response.reasoning_text.delta`
吐思考内容；若否，前端思考区按 architecture.md §4.4 的降级方案退化为工具调用时间线。

## 3. 语音模型可用性探测（2026-09-22）

对 Token Plan 上三个语音模型的可用性探测。凭据走的仍是 `src/bailian/token.ts` 的解析链路
（env → cc-switch 库）。

判定方法是先建立**对照**：未知模型名在 `/compatible-mode/v1/chat/completions` 上返回
`404 model_not_found`（结构化），已知可用模型返回正常 OpenAI 结构。只有落在这两者**之间**的
失败签名才能说明问题——单看一个 4xx/5xx 无法区分"模型不存在""payload 不合法""路由缺失"。

| 模型 | 在 `/compatible-mode/v1/models` 清单中 | 结论 |
|---|---|---|
| `qwen-audio-3.0-realtime-plus` | 是 | **可用**，完整往返跑通并产出音频 |
| `qwen-audio-3.0-tts-plus` | 是 | **不可用**：清单里有名字，但没有可用路由 |
| `qwen-audio-3.0-asr-flash` | 否 | **不可用** |

清单共 15 个模型，其中含 audio 的只有 `tts-plus` 与 `realtime-plus` 两个。

### 3.1 逐项探测结果

| 探测 | `tts-plus` | `asr-flash` | 对照（已知可用模型） |
|---|---|---|---|
| chat/completions + `modalities:["audio"]` | `500 InternalError UNKNOWN` | — | `200` + 正常 OpenAI 结构 |
| chat/completions + `input_audio`（真实音频） | — | `400` **空 body `{}`** | `400` **结构化错误**指明 payload 问题 |
| `/compatible-mode/v1/audio/speech` | `400 "url error, please check url！"` | — | 路径不存在 |
| `/compatible-mode/v1/audio/transcriptions` | — | `404 Api not found.` | 对 `fun-asr` 也是同一个 404 |
| 原生 `/api/v1/services/aigc/multimodal-generation/generation` | `400 "url error…"` | — | 该路径在图片生成上是好的 |

两条判读依据：

- **空 body `{}` 是"无路由"的签名。** 同形状的请求打到已知可用模型，会返回带 `message` 的
  结构化错误；打到 `asr-flash` 只得到一个空对象。
- **TTS 那句 `"url error"` 不可信。** 在两条不同路径、两种 payload 形状下拿到**逐字相同**的
  报错，真正的 payload 校验不会如此。它是这个网关的通用兜底，不是在抱怨请求里的 URL。

未排除项：无法断言不存在某个未文档化的请求形状能让 tts / asr 工作。但三个失败签名（空 body、
通用 `"url error"`、`500 UNKNOWN`）一致指向**路由缺失**而非 payload 不合法。

### 3.2 realtime-plus 是完整可用的

接入方式：

```
wss://token-plan.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime?model=qwen-audio-3.0-realtime-plus
Authorization: Bearer <key>
```

连接建立后服务端立刻发 `session.created`，默认会话配置：

```json
{"voice":"longanqian","modalities":["text","audio"],
 "input_audio_transcription":{"model":"fun-asr"},
 "turn_detection":{"type":"server_vad","threshold":0.5,"silence_duration_ms":800}}
```

`session.update` 被接受并回 `session.updated`。发一个文本 item 再 `response.create` 后实测收到：

```
response.audio.delta            × 2（各 25600 base64 字符）
response.audio_transcript.delta
response.done
```

合计约 51,200 个 base64 字符（≈38 KB 音频），即对"说：你好"的合成结果。会话空转 180s 会被
服务端以 `response_idle_timeout` 关闭，WebSocket close code `1007`。

**未提交音频有 30 秒上限。** 实测（2026-09-22，线上）：一次按住超过 30 秒不提交，端点对**其后每一帧**
append 都回一次

```
Input audio buffer exceeded maximum duration (30s). Please commit or clear the buffer.
```

按客户端的 43ms 分帧，这等于每秒约 23 条错误——从页面看就是"刷屏"。提交（`commit`）后缓冲区清空，
下一段重新计时。**这是端点契约，不是本项目的行为**，所以客户端把单段按住截在 25 秒
（`MAX_HOLD_MS`）并在到达时自动提交、停止送入。

**`101` 不能作为可用性证据。** 实测：不带任何 `model` 参数也能升级成功，
`/compatible-mode/v1/realtime` 这种大概率不存在的路径同样返回 101——升级是边缘无条件接受的。
只有升级之后服务端发的帧能区分：假模型名会在升级成功后立刻回
`{"code":"InvalidParameter","message":"Model not exist."}`。

### 3.3 对产品方向的含义

语音能力在这个 Token Plan 上**有，但入口只有 realtime 模型**：`modalities:["audio"]` 覆盖 TTS，
`input_audio_transcription`（内置 `fun-asr`）覆盖流式 ASR。语音相关能力应基于
`qwen-audio-3.0-realtime-plus` 设计，而不是找独立的 tts / asr 模型——后两者在目录里有名字
（或没有名字），但没有路。

附带事实：`fun-asr` 是真模型（realtime 会话自己声明用它做输入转写），但它在
`/compatible-mode/v1/audio/transcriptions` 上同样 404，说明该 OpenAI 路由整体未开，与模型无关。

## 4. 语音会话端到端（2026-09-22）

`/voice` 落地后的一次真实通话实测。链路：浏览器脚本 → `wss /api/voice`（Nest 中继）→
`wss /api-ws/v1/realtime`（百炼）。送进去的是本机 `say` 合成的一句中文，24kHz/16bit/单声道
裸 PCM，141,238 字节（约 2.9 秒）。

| 时刻 | 事件 |
|---|---|
| 128ms | WebSocket 连上（票据鉴权通过） |
| 5.7s | `ready`，服务端建出会话 #8 |
| 8.5s | 转写："请画一只站在树枝上的红隼。" |
| 9.7s | 模型请求 `generate_image` |
| 14.5s | 先出声："我来画一只站在树枝上的红隼。" |
| 29.2s | 产物落盘并推送：PNG 1,971,462 字节，`/api/assets/6/download` |
| 37.6s | 工具结果回灌后二次出声："画好了，这是一只站在树枝上的红隼，羽毛细节很清晰。" |

一轮共 37.6 秒（其中出图约 20 秒），语音回答 522,240 个 base64 字符（约 8 秒音频）。
落库确认：会话标题「语音会话」，用户消息 `mode` 为 `null`（无模式徽标），助手消息带 1 个产物
与 `toolCalls` 记录，刷新后可回放。

### 两个实测出来的实现契约

- **推挽必须显式要应答。** `turn_detection: null` 下 `input_audio_buffer.commit` 只提交音频，
  服务端**不会自动开口**——必须再发 `response.create`。第一次验证正是卡在这里：转写出来了，
  之后整条链路静默。发出 `response.create` 的时机取「收到转写」。
- **`ready` 要等约 5 秒**（首次建立上游会话的开销），所以浏览器在按下按钮后、会话就绪之前采到的
  音频不能丢——客户端为此加了待发队列。

### 浏览器侧在真实 Chrome 上确认的前提

`http://localhost:8848/voice`（`isSecureContext: true`）上实测：

| 项 | 结果 |
|---|---|
| `new AudioContext({ sampleRate: 24000 })` | `actual 24000`（默认上下文为 48000）——非默认采样率被兑现，**不需要手写重采样** |
| Blob URL 加载 AudioWorklet 模块 | 成功（本项目没有 CSP） |
| `AudioWorkletNode` 带 `channelCount:1, channelCountMode:'explicit', numberOfOutputs:0` | 构造成功。**默认节点是立体声**，不显式指定就会把立体声 PCM16 送进单声道契约 |
| 页面控制台 | 零错误零警告 |

### 4.1 线上实测（同日，经公网域名）

服务器与 Token Plan 主机同为阿里云，因此**文档里那个 65 KB/s 出站数字不适用于这条路径**——
这是本条记录要回答的核心疑问。同一条音频（24kHz 单声道 PCM16，137.9 KB ≈ 2.9 秒）经
`wss://try.kestrel.justwork.link/api/voice` 走一遍：

| 时刻 | 事件 |
|---|---|
| 290ms | wss 连上（经反代） |
| 670ms | `ready`，线上会话 #4 |
| 3.83s | 按**实时节奏**推完 137.9 KB（每 20ms 960 字节 ≈ 48 KB/s）并 commit |
| 4.37s | 转写准确 |
| 6.23s | 模型请求 `generate_image` |
| 21.69s | 产物落盘：PNG 2,170,045 字节 |
| 23.87s | 工具回灌后二次出声，`done` |

- **上行无积压**：推流期间 `bufferedAmount` 峰值仅 1305 字节。若链路扛不住实时音频，这个值会持续
  增长；实测不增长，说明 48 KB/s 的实时上行绰绰有余。推流 137.9 KB 用了 3.13 秒（实时应为 2.9 秒）。
- **下行**：收到 512,000 个 base64 字符（375 KB ≈ 8 秒音频）。
- 比本地快得多：`ready` 本地 5.7 秒、线上 0.67 秒；整轮本地 37.6 秒、线上 23.9 秒。差异主要来自
  本机到北京的网络往返。

**WS 穿反代的判定方法**（可复用）：向公网域名发一次**不带票据**的握手，看响应特征——

| 请求 | 响应 | 含义 |
|---|---|---|
| `/api/voice` | `401`，**响应体 0 字节** | 我网关手写的拒绝 → upgrade 已穿过 nginx 到达 Nest |
| `/api/voice/`（不匹配精确块） | `404`，响应体 73 字节 | 落回 `location /`，是 Nest 的 JSON 404 |
| `/api/chat`（GET 之） | `404`，响应体 71 字节 | 同上 |

空体 401 只可能来自网关（Nest 对 `GET /api/voice` 只给带 JSON 体的 404，nginx 不会回空体 401），
且带尾斜杠的变体正确地走了通用代理——**证明该 401 是路径特异的，放行是真实的**。

### 4.2 仍未覆盖

麦克风采集与播放需要真实点击与麦克风权限，**尚未验证**（脚本无法在不弹权限框的前提下触发
`getUserMedia`）。这是唯一剩下的空白，需由人在浏览器里按住按钮走一遍。

## 5. 多账号与超管：迁移与权限实测（2026-09-22）

本次改动加入 `users.role` 并引入内置超管账号（当时凭据写在代码里，2026-09-27 起改为
`SUPER_ADMIN_*` 环境变量注入，见 [deployment.md](deployment.md) §5）。为了不动正在运行的服务、也不动真实库，
实测跑在**真实库的一份拷贝**上：另起一个进程（`PORT=8899`、`DATABASE_FILE` 指向拷贝），
拷贝的 schema 恰好是改动前的形态——`users` 只有 `id/username/password_hash/created_at`，
`users` 表里也只有 `admin` 一行。

| 项 | 结果 |
|---|---|
| 启动时补列 | `users` 变为 id, username, password_hash, **role**, created_at；`admin` 自动落到 `user` |
| 内置超管 | 启动时创建超管账号，role = `super` |
| 老账号仍可用 | `admin` / `123456` 登录 302 → `/` |
| 超管登录 | 超管账号 + 当时的内置口令登录 302 → `/` |
| 管理入口 | 超管的对话页与语音页出现指向 `/admin` 的链接，普通账号不出现 |
| 全站会话 | 超管在 `/admin/conversations` 看到 8 个历史会话及其归属；普通账号访问任何 `/admin` 页一律 302 回 `/` |
| 只读回放 | 超管打开 admin 的会话：转写、思考、工具调用、图片都在，**没有输入框、不加载 `/assets/main.js`** |
| 超管读 API | `/api/conversations/1/messages` 返回 2 条；`/api/assets/6/download` 200 |
| 普通账号读同一份 | messages 空数组、资产 404、会话列表里没有 admin 的会话 |
| 写路径不放宽 | 超管带他人 conversationId 发消息时仍新建自己的会话，他人会话保持空 |
| 删自己 | `/admin/users/2/delete` → `?notice=self`，账号仍在 |

合计 28 项断言全部通过，含浏览器里真按一遍改密表单（旧密码随即失效、新密码可登录）。
同步核对：**真实库与正在运行的服务未被改动**（仍是改动前的 schema、只有 `admin`）。

### 5.1 搬家后旧资产的绝对路径已失效 —— 已修

`assets.file_path` 存的是**绝对路径**。项目从 `playground/kestrel-studio/` 搬到当前位置后，
搬家前写入的 5 条资产路径全部指向旧目录，于是下载端点对**属主本人**也返回 404——
实测 asset 1 对 `admin` 与超管同为 404，而搬家后写入的 asset 6 对属主与超管同为 200。

根因与改法见 [implementation-notes.md](implementation-notes.md) §7：列改为只存文件名，
读取时按当前 `STORAGE_DIR` 解析；旧行的绝对路径若还在原处则原样沿用。修完在**同一份真实库拷贝**上复测：

| 资产 | 改前 | 改后 |
|---|---|---|
| 1（搬家前，2,009,073 B 图片） | 404 | **200**，bytes 与库中记录一致 |
| 2、3（搬家前图片） | 404 | **200**（1,998,397 / 1,378,534 B） |
| 4、5（搬家前视频） | 404 | **200**（7,390,473 / 5,519,686 B） |
| 6（搬家后图片） | 200 | 200（1,971,462 B） |

历史会话里的图片与视频现在都能打开。

### 5.2 Chromium 的密码表单告警：隐藏的账号字段不算数

管理页最初每行的改密表单只放一个密码框，Chrome 控制台给出
`[DOM] Password forms should have (optionally hidden) username fields for accessibility`。
提示语里的 "optionally hidden" 具有误导性，于是在真实页面上注入变体逐一对照——
先 reload 让计数归零，再看总数是否增长：

| 注入的表单 | 计数变化 | 结论 |
|---|---|---|
| 只有密码框 | +1 | 触发 |
| `type="hidden"` 账号 + 密码 | +1 | **仍触发** |
| 可见（`readonly`）账号 + 密码 | 0 | 不触发 |

所以表单里必须有一个**可见**的账号名输入框。最终把改密从账号列表移进单账号管理页
（`/admin/users/:id`），那里放一个 `readonly` 的账号名——既满足判定，也不再让表格挤成一团。
管理页至此控制台**零条目**（对话页与只读回放页仍是那条既有的 lazy-image issue，见 §2 覆盖范围说明）。

## 6. 历史会话软删除（2026-09-22）

> 2026-10-08 更新：二次确认的**呈现形式**已从行内「确认 / 取消」改为原生 `<dialog>`
>（理由见 architecture.md §12.1）。下面「点「删除」」那一行的界面描述是改动前的；
> 请求行为、属主/超管可见性与库里的行数不受该改动影响。

删除会话落地后的一次真实浏览器实测（真实库拷贝 + 独立进程，`PORT=8899`）。
要判定的三件事是**属主看不见、超管看得见、数据一点没少**，所以三条都实测：

| 步骤 | 结果 |
|---|---|
| 普通账号打开「历史会话」 | 8 个会话，每行一个「删除」 |
| 点「删除」 | 该行变成「删除？ 确认 取消」，**未产生任何请求**（二次确认的第一步） |
| 点「确认」 | 该行从列表消失，其余 7 行原样，下拉保持展开，当前打开的会话不受影响 |
| 控制台 | 只有那条既有的 lazy-image *issue*，无新增条目 |
| 超管 `/admin/conversations` | 8 个会话都在，被删的那条带「已删除 · 2026/09/22 17:36」标记 |
| 超管的只读回放 | 整段历史、思考过程、工具调用、图片全部照常渲染，横幅注明「已于 … 被用户删除」 |
| 库里 | 会话行仍在且 `deleted_at` 有值；2 条消息、1 条资产行与磁盘文件全部保留；8 个会话里 7 个未删 |

## 7. 线上语音一直停在「正在连接…」的排查与修复（2026-09-22）

**症状**：在线上按住说话，页面永远停在「正在连接…」，控制台零错误零警告。

**排查路径**（值得复用，因为服务端在成功路径上完全不出声，全靠外部证据定位）：

| 证据 | 读到的信息 |
|---|---|
| 容器日志（`docker logs`）| **一条语音相关记录都没有**。语音代码只在告警/出错时打日志，说明服务端既没报错、也没走到任何分支 |
| 1Panel 站点访问日志 | `POST /api/voice/ticket` → **201**；`GET /api/voice?ticket=…` → **101，`$body_bytes_sent = 2`** |
| 对照：14:16 那次成功通话 | 同样 101，但 `$body_bytes_sent = 513123`（音频下行） |

`101` 说明升级成功、nginx 与网关都正常（顺带纠正：早先我用 curl 探 `/api/voice` 拿到 404/72 字节，那是**普通 GET** 的正常响应——非 upgrade 请求本来就不走 upgrade 事件，当时把它当成代理嫌疑是误判）。**2 字节**才是指向根因的数字：整条会话只发了一个 WS 关闭帧，服务端一个字节都没发。

**根因**：`web/scripts/voice.ts` 的客户端**从未发送 `start` 帧**。`ensureSocket()` 设了 `onmessage`、`onclose`，漏了 `onopen`。而服务端的 `VoiceConnection.receive()` 只在 `case 'start'` 里开会话（建上游会话 → 回 `ready`），于是：socket 健康、服务端不知道有通话、浏览器 `ready` 永远为 false → 状态永远停在「正在连接…」。

这个失败**两端都不出声**：客户端不进错误分支（socket 是好的），服务端不进告警分支（什么都没发生）。

**为什么之前没被发现**：线上那次语音验证（§4.1）是用 Node 脚本跑的，脚本自己发了 `start`；浏览器这条路径当时只验过"控制台干净"——而它干净恰恰是因为失败是静默的。真正的麦克风路径当时标注为未验证（§4.2）。

**修复**：
1. 客户端在 `onopen` 里发 `{ type: 'start' }`。
2. `ensureSocket()` 对 `CONNECTING` 状态的 socket 也复用，避免重复开会话（防御性，当前按钮守卫下不可达）。
3. `ready` 到达且仍按住时把状态从「正在连接…」改成「正在听…」。
4. **服务端补上会话开合日志**：`Voice session opened (conversation N, user M)` / `Voice call ended (conversation N)`，上游建连失败也记 warn。这次难查就是因为成功与"什么都没发生"在日志里长得一样。

**验证**：
- 新增 `web/scripts/voice.test.ts`（jsdom + 假 socket），断言帧序列而非"没报错"：打开即发 `start`、ready 前音频排队/ready 后补发、ready 前松开补发 `commit`、ready 后立刻 `commit`。注入变异（去掉那一帧）后 **4 条测试失败**，证明它抓得住这个 bug。
- 本地实例按同一帧序列实测：`start` → **0.3 秒收到 `ready`**（会话 10），按实时节奏推 2 秒音频 + commit，**无 error 帧**。
- 端点拒绝突发：一次性推 2.4 MB（非实时节奏）会被回 error 帧，与"必须实时节奏"的既有结论一致。

**仍未验证**：真实麦克风采集（§4.2 的空白），以及线上浏览器端的复发验证——修复已部署，等真人按一次即可从新的会话日志确认。

### 7.1 后续：多轮之后「一堆 error」（同日）

修复 `start` 帧之后用户在线上真机测试，报「几轮之后打印了一堆 error」。这一轮**只有外部证据**能查，
因为那几条路径当时都不打日志：

| 证据 | 读数 | 推论 |
|---|---|---|
| 容器日志 | 全量只有 4 行 `Voice`（一次开合，50 秒），**零 warn** | 不是上游 socket 出错、不是上游主动关闭、不是会话建立失败、不是回灌超时 |
| 站点访问日志 | 该会话 `101`，**`$body_bytes_sent = 1091162`**（1.1 MB）| 下行 1.1 MB ≈ 二十多秒语音 → **语音回答确实出来过，前面几轮是好的** |

于是只剩三条**会下发 error 帧但不打日志**的路径：上游的 `failed` 事件转发、静默守卫
（「没有听清，请再说一次」）、工具轮次上限（「工具调用轮次过多」）。**具体是哪一条、上游的原话是
什么，当时无从得知**——因为转发给浏览器的消息没有落日志。

本地用 `say` 合成中文语音复现三轮（真实帧序列 + 实时节奏）：**零 error**，说明多轮本身没问题。

处理：
1. 补齐日志——上游 `failed` 的**原话**、静默守卫触发、工具轮次上限、每轮的
   `Voice turn committed (N audio frames)`、以及 `Voice transcript: …`。最后两条合起来正好回答
   「麦克风没送出」与「端点拒收」这两个从浏览器看完全一样的故障。
2. 收敛刷屏：一次上游失败只往浏览器发**一行**，重复的只记日志带计数。

**定因（同日，用户贴出原文）**：上游原话是
`Input audio buffer exceeded maximum duration (30s). Please commit or clear the buffer.`
——即 §3.2 记的那条契约。一次按住超过 30 秒不提交，上游随后对每一帧 append 都回错，于是刷屏。
与那 1.1 MB 下行也吻合：前面几轮短句正常出声，某轮说久了才炸。

处理：客户端把单段按住截在 25 秒（到点自动提交 + 提示用户"还有话请松开再按一次"），
收到 error 后本次按住不再送音频；中继在上游拒绝后不再往缓冲区里灌，且一次失败只往页面发一行。
对真实端点复测（故意推送 35 秒不提交）：**页面只收到 1 条 error**（改前是数百条），
服务端日志写下 `Voice turn failed: Input audio buffer exceeded maximum …`。

## 8. 二期：提交与订阅分开、对话级事件流续传实测（2026-10-21）

环境：开发机本地，独立烟测进程（`PORT=8899`，scratch 目录里的独立 DB 与 storage，
不碰常驻实例与真实库），凭据走 cc-switch 解析链，**供应商是活的**。

### 8.1 链路事实（`mode=chat` 一轮，真实 LLM）

| 步骤 | 实测结果 |
|---|---|
| `POST /api/v1/conversations` | `201 {conversationId:1}` |
| `POST /api/v1/conversations/1/turns` | `202 {conversationId:1, messageId:2}`——响应不带任何流 |
| 同一会话立刻二次提交 | `409 {message:"该会话已有轮次在进行中"}` |
| 事件日志终态（直查 SQLite） | `[(1,user_message),(2,turn_started),(3,text),(4,text),(5,text),(6,turn_done)]`；assistant 行已收尾为非空正文 |
| 完成后 `GET .../stream?from=0` | 首帧 `connected`（**无 id**），随后 `id: 1` 的 `user_message`——落库部分的日志重放，跨进程重启也能放 |
| 重连带 `Last-Event-ID: 2` | 第一条业务帧是 `id: 3` 的 `text`——续传游标生效 |
| SSR 契约 | 轮次进行中的页面带 `data-conversation-id="1"` `data-start-seq="1"`：追及游标恰落在那一轮 `turn_started` 之前，未完的助手占位行不画（由流重建） |

结论：§4.2（architecture）描述的「先落日志再扇出、seq 会话内自增、
Last-Event-ID 重放、无游标连接按未完轮次定位」全部按设计工作。

### 8.2 质量门与覆盖面

- `make typecheck`（三份 tsconfig）+ `make test`：**30 个文件 510 个用例全绿**。
- 新增/重写的测试面：事件日志服务（seq/重放/断档/裁剪/跨会话隔离）、HTTP 面
  （202/409/404 写规则、流的重放与 resync、追及游标、启动恢复）、SSR 契约
  （`data-start-seq`、未完占位行不画、`/api/v1` 资产 URL）、客户端脚本（轮次不
  本地拼装、按 messageId 键控渲染、锁/解锁、resync 触发重载、新建会话以 from=0
  先开流再提交）。

### 8.3 本轮未覆盖（诚实边界）

- **图片/视频资产事件未经真实供应商端到端**：`asset` 事件在流上的行为由单元与
  HTTP 测试覆盖，§2（2026-09-20）的出图实测证据仍写于旧轮次流形态，引用者注意。
- **浏览器控制台未复查**：本轮无 Chrome DevTools MCP 可用；客户端行为由 jsdom
  帧序列测试覆盖（教训使然，见 implementation-notes §8），但控制台零噪音契约
  待下次人工在浏览器里过一遍。
- **重启进程后的续传**未经真机演练（烟测里流在日志落库后新开连接即重放，等价
  于跨进程读表）；`resync` 真实触发依赖 2000 条保留窗口，由测试模拟。
- 语义变化确认：**断开连接不再中止轮次**（设计如此），旧的 `req.close → abort`
  路径随 POST 流退役。

## 9. Rust 实现（`rust/`）的离线验证（2026-10-27）

这一节的证据全部来自**离线、无服务**的环境：本机无出站网络，沙箱禁止 bind
套接字（`PermissionDenied`），因此路由层测试用 `tower::ServiceExt::oneshot` 驱动
路由表而不是起真 HTTP 服务，帧层测试用 `tokio::io::duplex` 在进程内跑真字节流。
引用时注意它与 §1–§8 的差别：那些是**活的端点**，这里是**移植等价性**。

### 9.1 计数

- `cargo test`：**157 passed / 0 failed / 2 ignored**（ignored 的两条是 §9.2 的跨实现
  校验，一条依赖本地 `data/kestrel-studio.db`，一条依赖 `node` + `bcryptjs`）。
- `cargo check --tests`：无 error。release 构建**未跑过**（`make rust-build` 只做
  了 `make -n` 解析验证）。
- 迁移提交：`ac6abe4` 主体 → `a653d54` 语音传输 → `7ec90a0` 通话状态机 →
  `0cf86e7` 去占位 → `5b075e5` 派发测试 + 工具名顺序 → `bc8a3c5` 清未用导入 →
  `d0d71eb` 忽略 `rust/target/`。

### 9.2 口令：与两套独立实现对照

`bcrypt` 不在离线 registry 里，手写实现（`auth/bcrypt.rs`）的正确性用**两个无关实现**
判：仓库自带的 `bcryptjs`（写入库里哈希的那个）与 `/usr/sbin/htpasswd`（Apache
crypt_blowfish，产出 `$2y$`）。

```bash
node -e "const b=require('bcryptjs');console.log(b.hashSync('hunter2',5))"
/usr/sbin/htpasswd -nbB t1 'hunter2'
```

8 条向量（含空口令、非 ASCII、恰 72 字节与 80 字节、`$2$` 旧版、`$2y$`）全部通过。
反向也验：Rust 写的哈希交给 `bcryptjs.compareSync` → `true`，错口令 → `false`，
故 Nest 与 Rust 可共用一个库。**本地库两个真实账号**（`admin`、超管 `tww316`）以
只读连接验证仍可登录，且库文件 mtime 未变。

一处值得记下的口径：口令超过 72 字节时两个实现都把 72 之后的字节**忽略**（每个
key pass 恰好消耗 18 个字 = 72 字节且偏移复位）。`'x'×72` 与 `'x'×80` 同哈希、
`'x'×71` 不同——由 `bcryptjs` 实测确认，不是推断。

### 9.3 握手签名：以 `openssl` 为参照

`Sec-WebSocket-Accept` 的公式（`base64(SHA1(key + 258EAFA5-…B11))`）用系统
`openssl` 现算对照，避免凭记忆写 RFC 样例：

```bash
printf '%s' 'x3JJHMbDL1EzLkh9GBhXDw==258EAFA5-E914-47DA-95CA-C5AB0DC85B11' \
  | openssl dgst -sha1 -binary | base64      # HSmrc0sMlYUkAGmm5OPpG2HaGWk=
```

（我先前按记忆写下的 `dGhlIHNhbXBsZSBub25jYXQ=` 对应值是**错的**，实测为
`C0iGEECXJdPQ1Dfevh4d/wLzuZw=`；测试已改用 openssl 生成的对照值并附命令。）

### 9.4 等价性被测试钉住的语义

- 事件流：`from=0` 重放、`Last-Event-ID` 优先于 `?from=`、断档时 `resync` 先于
  `connected` 且不填充缺失、**重放帧的 `data:` 与库中 `payload` 逐字节相同**。
- 可见性：属主/他人/超管三角矩阵；软删除对属主不可读、对超管仍可读、行与文件
  都在；`can_read` 与 `is_owned_by` 的宽窄差异。
- 语音：帧泵在 duplex 上验（掩码强制、半帧缓冲、EOF 视作关闭、下行不掩码、
  控制帧不分片、超限早拒）；工具轮次验 `tool_output → create_response` 的先后与
  5 轮上限。
- 模板：页面必含 id 是**从 `web/scripts/main.ts` 解析 `requireElement` 得到**的，
  不是手抄清单（17 个），改客户端会红。

### 9.5 本轮未覆盖（诚实边界）

- **无端到端**：不起服务，`GET /`、`/api/v1`、语音的真机链路一次都没走过；
  浏览器控制台零错误零警告这条全局契约未复查。
- **`hyper::upgrade::on` 接管与 `tokio-rustls` 真拨号未验证**：构件各有测试，
  组合处没有。语音现在能完成 101 并接管，但上游未接时以 `Close` 结束。
- **`dispatch_pending` 只用"未知工具"路径驱动**：真实出图/出视频的派发依赖活供应商。
- **release 构建未跑过**；`Dockerfile` 无 Rust 阶段，服务器零下载前提下如何喂 crate
  尚未解决。

## 10. 出站网络与 WASM 客户端可行性探测（2026-10-30）

为「移除 Nest/Node 技术栈、全仓改 Rust」这个决策做的两项探测。都是在
`/tmp` 的临时探针工程里跑的，未触碰 `src/` 与 `rust/`。

### 10.1 开发机有出站网络（纠正 architecture.md §13.1 的前提）

```bash
$ cd /tmp/kestrel-net-probe && cargo add tera --dry-run
    Updating crates.io index
      Adding tera v2.4.0 to dependencies
```

本地 registry 缓存清点（只数命中数，不解析内容）：

```bash
$ ls ~/.cargo/registry/cache/*/ | wc -l        # 606 + 368
$ ls ~/.cargo/registry/cache/*/ ~/.cargo/registry/src/*/ \
    | grep -icE '^(tera|minijinja|handlebars|askama|maud|leptos|yew|dioxus|sailfish)'
0
```

结论：**「离线 registry 缺件」是执行环境的网络沙箱所致，不是这台机器的属性**。
§9 里那些手写层的动机描述需要按此理解，真实约束只剩服务器侧零下载（deployment.md §1）。

### 10.2 Rust→WASM 客户端：能编，代价是体积

`rustup target add wasm32-unknown-unknown` 成功（该 std 组件约 15 MB，本机对
`static.rust-lang.org` 实测只有 ~50 KB/s，下载用了约 4 分钟，需按此预估等待）。
探针依赖：`wasm-bindgen 0.2.129` / `js-sys 0.3.106` / `web-sys 0.3.106`（90 个
feature）/ `wasm-bindgen-futures 0.4` / `pulldown-cmark 0.13`。
`cargo check --target wasm32-unknown-unknown` 全部依赖编译通过，探针本身 0 error。

本项目客户端真正需要的 API，实测在 web-sys 里都有**类型化绑定**（不是只能 Reflect）：

| API | web-sys 绑定 | 备注 |
|---|---|---|
| `EventSource` + message | `EventSource::new` / `add_event_listener_with_callback` | 现客户端订会话事件流的主干 |
| `WebSocket`（语音） | `WebSocket::new` / `send_with_str` | |
| `getUserMedia` | `Navigator::media_devices()` → **返回 `Result`**（非安全上下文会抛） | `MediaTrackConstraints::set_channel_count(&JsValue)`，联合类型走 `JsValue` |
| `AudioContext` / `AudioWorklet` | `AudioContext::new` / `audio_worklet()` / `add_module(&str) -> Promise` | **但 worklet 内的 `AudioWorkletProcessor` 子类必须是 JS**：worklet 全局作用域不挂 DOM/wasm 宿主，`registerProcessor` 只能由 JS 调 |
| `dataset`（`data-conversation-id` 等交接面） | `HtmlElement::dataset()` | **必须开 `DomStringMap` feature**，否则报 no method |
| canvas `toBlob`、`URL.createObjectURL`、`localStorage`、`history` | 均有绑定 | 下载弹窗那条重编码路径可用 |

体积账（同一台机、同一 gzip 口径）：

| 产物 | raw | gzip |
|---|---|---|
| 探针 `main.wasm`（含 90 个 web-sys feature + pulldown-cmark，未经 `wasm-opt`） | 1.4 MB | **338 KB** |
| 现 `public/assets/main.js`（内含 marked） | 59 KB | 18 KB |
| 现 `public/assets/voice.js` | 6.5 KB | 3 KB |
| 现 `public/assets/main.css` | 16 KB | 3.5 KB |

即客户端从约 24 KB gz 涨到 200 KB gz 量级（未 `wasm-opt`、未裁剪 feature、未按页面
拆模块）。可缓解，但**方向上是一次 10 倍的首屏载荷交换**，这是选型 B 的定价。

### 10.3 工具链现状

```bash
$ brew info wasm-pack wasm-bindgen        # 二者均"available"，本机都未安装
```

`wasm-bindgen` formula 停在 0.2.111（且 homebrew 已标注 deprecated、改用 wasm-pack），
而 crate 解析到 0.2.129——**wasm-bindgen CLI 与 crate 版本必须配对**，因此不能拿
brew 那个 CLI 用；要么装 `wasm-pack`（它按 `Cargo.lock` 里的版本自取匹配 CLI），
要么 `cargo install wasm-bindgen-cli --version 0.2.129`。

同日更新：`wasm-pack 0.15.0` 已装好（brew bottle 只有 2.5 MB）。但 **wasm 的 JS glue
产物与 `wasm-opt` 后体积仍未实测**——`client/` 还没有可导出的 `#[wasm_bindgen]` 接口
（DOM 层未移植），所以还没有东西可以交给 wasm-pack 生成。

## 11. P2 交叉编译与 P3 客户端核心（2026-10-30）

### 11.1 目标机事实

`ssh lavo-test` 只读查询：

```
x86_64
CentOS Linux release 7.9.2009 (Core)
Docker version 26.1.4
Mem: 3645 MB total / 2382 MB available      df: / 99G used 45G avail 50G
```

架构与发行版共同决定交叉编译的目标三元组：`x86_64-unknown-linux-musl`——CentOS 7
只有 glibc 2.17，动态链接的新二进制在宿主机上跑不了，静态 musl 不依赖 libc。

### 11.2 静态二进制：已产出

```bash
$ make rust-build-linux
$ file -b /tmp/kestrel-studio-linux/kestrel-studio
ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, stripped
```

- 体积 **9.2 MB**。deploy 归档因此从旧管线的约 300 MB（node_modules）降到几 MB。
- 过程与两次链接失败的根因见 [implementation-notes.md](implementation-notes.md) §14：
  C 侧（`ring` 的 C/asm、`libsqlite3-sys` 的 bundled SQLite）经 zig 编译是通过的，
  失败在链接的方言与启动文件归属。
- **未验证**：这个二进制**从未运行过**（本机 arm64/darwin 跑不了 x86-64 ELF），
  `FROM scratch` 镜像也**没有构建过**——本机 Docker daemon（colima）未启动，
  启动服务不在本次授权范围内；线上部署同样未执行。
- 已知副作用（设计如此，architecture.md §13.2）：`connect.sid` 的签名算法两侧不同，
  旧 cookie 被拒绝 → **切流时在线用户重新登录一次**。

### 11.3 协议单一来源：`proto/` 已落地

语音帧类型从 `rust/src/voice/protocol.rs` 移到新 crate `kestrel-proto`，服务端与客户端
编译同一份定义；`protocol.rs` 保留**线名测试**（钉住 `conversationId` 这类浏览器实际
监听的名字），`TaskKind` 一并迁入，`rust/src/task/mod.rs` 改为
`pub use kestrel_proto::TaskKind`，全树仍按 `crate::task::TaskKind` 引用。

```bash
$ cargo test --manifest-path rust/Cargo.toml
test result: ok. 159 passed; 0 failed; 2 ignored
```

比 §9.1 记的 157 多 2 条，来自语音升级交换那条后补的测试（提交 `21da143`），不是本次
改动带来的。警告数 **34，与改动前一致**（中途引入的一条 unused import 已修掉）。

### 11.4 客户端核心状态机：13 条测试在宿主目标通过

新 crate `client/`（`kestrel-client`）。`voice_core` 是 push-to-talk 的纯状态机：不碰
时钟、不碰 socket；输入是 `now_ms` 与 `kestrel_proto` 的帧，输出是 `Effect` 值。

```bash
$ cargo test --manifest-path client/Cargo.toml
test result: ok. 13 passed; 0 failed
```

覆盖的正是 jsdom 套件当年承担的顺序断言（§7 那次故障的形状）：开机首帧必须是 `start`；
`ready` 前的音频排队、`ready` 后按序补发；`ready` 前松手时 `commit` 排在补发之后；
25 s 上限在 `ready` 才起算；到点自动 `commit` 且此后不再灌音频；`error` 帧封锁本次按住
剩余音频、松手不再二次 `commit`；非法与未知帧被忽略而非 panic；`busy` 期间拒绝第二次
按下；`ready` 订上会话事件流、断开时退订并提示"再按一次继续"；离开页面发 `stop`；
`asset` 帧转成预览。

**未验证**：`client/` 还没有 DOM 层——`web/scripts/voice.ts` 的 socket / 音频 / UI 那部分
尚未移植，WASM 产物也还没生成过。

### 11.5 WASM 产物的体积：markdown 引擎才是那面墙（同日补，含一次自我更正）

`client/` 加了三个**真导出**（不是桩）：`renderMarkdown`（`pulldown-cmark`，取代 `marked`）、
`hasMicrophone`、`buildInfo`。

**先记一条被我引用错过的数字**：本节初稿写的是"303 KB gz"，那是 §10 那个 90-feature 探针
**未经 `wasm-opt`** 的体积，被我在会话里当作客户端地板引用了。实测（`wasm-pack build
--release --target web`，含 wasm-opt）比它低得多：

| 产物 | raw | gzip |
|---|---|---|
| `kestrel_client_bg.wasm` | 222,694 B | **95,243 B** |
| `kestrel_client.js`（glue） | 10,193 B | 3,014 B |

然后把 `render_markdown` 与 `markdown_options` 两个函数连同 `pulldown-cmark` 依赖一起删掉，
在 `/tmp` 的副本上重编（仓库未改）：

| 同一个模块，无 markdown 引擎 | raw | gzip |
|---|---|---|
| `kestrel_client_bg.wasm` | 19,357 B | **8,672 B** |

**结论与 §4.1 的原判断相反**：那 95 KB gz 里 **约 74 KB 是 `pulldown-cmark`**，不是
"panic 机制 + 绑定层"这种不可压缩的 WASM 开销。

> **2026-11-03 补：本节有两个数字容易被混用，分清再引。** 95.4 − 21.1 ≈ **74 KB** 是
> "换成 `pulldown-cmark` 要多付多少"（对照组是现在的子集）；95.4 − 8.7 ≈ **86 KB** 是
> "markdown 这一整块要付多少"（对照组是**完全不带**渲染器）。前者才是"为什么不用 pulldown"
> 的答案；`rust/client/Cargo.toml` 的注释此前引了后者，已改正。据此把 markdown 换成**服务端已有的
手写子集**（抽成 `md/` crate，SSR 与客户端编同一份，见 architecture.md §13.4），
`pulldown-cmark` 从客户端依赖里删除，同日重测：

| 产物 | raw | gzip |
|---|---|---|
| `kestrel_client_bg.wasm`（含共享 markdown + `voice_core` + serde_json） | 46,885 B | **21,133 B** |
| `kestrel_client.js`（wasm-bindgen glue） | 10,711 B | 3,281 B |
| **合计** | | **约 24.4 KB gz** |
| 对照：现客户端 `main.js` + `voice.js` | 65,617 B | **20,999 B** |

即 **"WASM 首屏贵 10 倍"这句更正后的结论是：在当前已移植的范围内，B 的体积与 JS 持平**
（24 KB gz vs 21 KB gz），差值来自 WASM 侧还要在浏览器外做一次 JSON 解析与状态机，
而 `marked` 本身也占了 JS 里 18 KB gz 中的大头。

**这条结论的边界，不要越过**：

- 客户端的 **DOM 层还没写**，所以 21 KB gz 里不含任何 `document.createElement` /
  `WebSocket` / `EventSource` / `fetch` 的代码。绑定层是按需生成的，真实增量**未测**，
  估计在几十 KB raw 量级——但这是估计，不是数据。
- `wasm-opt` 由 wasm-pack 默认执行；手工 `cargo build --target wasm32` 出来的数字要大 5 倍，
  引用时务必区分（§11.5 初稿就误引过它）。
- markdown 子集**不含 GFM 表格与删除线**，`marked` 是支持的。切换后助手若画表格，
  会显示成竖线原文。这是产品可见的行为变化，需要决策（补子集语法，或接受），
  记在 roadmap.md §4.1。（**2026-11-03 已按第一个选法处理**，实现与复测见 §16；
  本条是当时的口径描述，保留原文不删。）

复测方法（本机可做，属构建不属起服务）：
`cd client && wasm-pack build --release --target web --out-dir /tmp/...`，
对 `kestrel_client_bg.wasm` 与 `kestrel_client.js` 各跑一次 `gzip -9 | wc -c`。

## 12. 语音页 WASM 客户端落地（2026-10-30）

一句话更正前面的载荷结论：桩量的"持平"不成立，真实语音页把模块推到约 85 KB gz
（整站现客户端 21 KB gz）。**这条到此为止，不再作为决策输入**——按用户判断，体积不是这个
迁移该纠结的量。已保留 `panic = "abort"` + `codegen-units = 1`（顺手省 1.7 KB gz）。
「不再作为决策输入」不等于「数字可以烂在文档里」：对话页两项补完后这个数字又涨了一次，见 §13.9，§4.1 那句「实测持平」现在引用的是过期测量。

### 12.2 代码到位到哪一步（重要：运行时一次都没验过）

已落地并通过的：

- `client/src/wire.rs`：票据换取 + WebSocket + 帧进出。**`open()` 在装 handler 之前
  就把 socket 放进 slot**，因为 core 收到 `Opened` 后立刻要发 `start`；
- `client/src/audio.rs`：`AudioContext(24kHz)` + worklet + 采集/播放队列。
  **没有任何 `RefCell` 借用跨过 `await`**——松手时权限框还挂着是常态，
  带着借用去 `getUserMedia` 会在回来时 panic；
- `client/src/voice_page.rs`：元素查找、按住/松开、250 ms tick、Effects 解释器；
- `web/public/capture.js`：worklet 处理器，就是 §4.1 说的那块删不掉的 JS 地板
  （`Vite` 的 publicDir 会拷到 `public/capture.js`，`make build-web` 之后出现）；
- `client/src/pcm.rs`：base64 与 PCM16 小端的纯函数，**7 条宿主测试**。
  写测试时我把 32767 当成 `0x3FFF` 拼错过一次，是测试把我抓住的（见 §13）；
- 契约测试新增 1 条：`rust/src/view/tests.rs::the_voice_page_carries_every_element_the_wasm_client_requires`
  从 `client/src/voice_page.rs` 源码里读出页面查找的 id，断言 `/voice` 骨架逐个带着——
  这是原来"从 `web/scripts/main.ts` 解析 `requireElement`"那条机制的 Rust 版本。

```
$ cargo test --manifest-path rust/Cargo.toml      # 160 passed / 0 failed / 2 ignored（警告 34，与基线一致）
$ cargo test --manifest-path client/Cargo.toml    # 21 passed / 0 failed
$ cargo check --target wasm32-unknown-unknown --manifest-path client/Cargo.toml   # 0 error 0 warning
```

**未验证的，说清**：

1. `initVoice` 导出了但**没有任何地方调用**——`/voice` 骨架仍然引 `/assets/voice.js`
   （`rust/src/view/templates.rs` 未改）。切换点是 P4 的事，P1 未通过前不动。
2. 语音页在浏览器里的行为**一次都没跑过**：本机不启动服务、不启动 Docker（CLAUDE.md），
   所以采集、播放、松手提交、25 s 截断、断线重连提示，全部只有编译与纯逻辑两层保证。
3. 麦克风真机验证仍是从 Nest 时代起就挂着的那一条（roadmap.md §3）。

### 11.6 本机不可验证的部分，去处已定

本机禁止启动 Docker、禁止为本机起服务做测试（CLAUDE.md），因此上面这些**运行时的真相**
（容器能否起、`/login` 是否 200、SSE 首帧、静态资源 ETag）不在本机取得。
唯一去处是**目标主机上的灰度实例**，由用户授权后人工执行。曾写过一份远端 CI 烟测定义，
按要求删除，且删除前从未运行过——**不要把它当作已存在的验证手段**。

## 13. 线上切至 Rust 服务端（2026-11-02）

一句话：roadmap.md §4.3 的 **P1 判据部分通过**。交付链、容器启动、鉴权、静态资源、
公网反代、一轮真实流式对话都已在 Rust 二进制上跑通；出图 / 视频 / 语音 / `/admin` 内容 /
SSE 断线续传这五条仍未走。以下全部来自 `ssh lavo-test` 的只读探测。

### 13.1 容器与进程事实

| 项 | 事实 |
|---|---|
| 容器 | `kestrel-studio` `Up`，`0.0.0.0:8848->8848` |
| 镜像 | `kestrel-studio:latest` **10.2 MB**（同机留下的最后一个 Nest 镜像是 1.27 GB） |
| 入口 | `ENTRYPOINT [/app/kestrel-studio]`；`.build/linux-x86_64/kestrel-studio` 9,701,056 B，`file` 判定 `ELF 64-bit, statically linked, stripped` |
| 启动日志 | `Bailian credentials resolved` / `expired sessions cleared=0` / `kestrel-studio listening on http://localhost:8848` |
| 占用 | `docker stats` RSS **1.219 MiB**（3.56 GiB 限额的 0.03%） |
| 当天部署 | 三次：镜像 `13:24:03` / `13:45:00` / `13:47:03`，在跑的是最后一个 |

静态 musl 二进制在 CentOS 7（glibc 2.17）上直接运行，印证了 deployment.md §1 那条
「静态链接是承重选择」；`scratch` 里没 `/etc/ssl/certs` 也没影响——出站取凭据正常。

### 13.2 容器内 HTTP 探测

```bash
for p in /login /assets/main.css /capture.js /wasm/kestrel_client_bg.wasm /api/voice/ticket /admin; do
  ssh lavo-test "curl -s -o /dev/null -w '%{http_code} %{size_download}B' http://127.0.0.1:8848$p"
done
```

| 路径 | 结果 | 含义 |
|---|---|---|
| `/login` | 200，909 B，`text/html` | SSR 页面在 |
| `/assets/main.css` | 200，16153 B，md5 `a1ba5ddb932569db087f8b95615aaef1` | 与开发机当天 `make build` 产物**逐字节相同**，自带静态托管可用 |
| `/capture.js` | 200，1687 B | WASM 客户端那块「删不掉的 JS 地板」已上线 |
| `/wasm/kestrel_client_bg.wasm` | 200，340600 B，`application/wasm` | 产物已随 `public/` 上线，但模板没有引用它（P3 的已知状态） |
| `/api/voice/ticket` | 405 | 路由已挂载，GET 不接受，符合预期 |
| `/admin` | 302 → `/login` | 角色守卫在 |

### 13.3 公网入口（openresty 反代）未因切换而回归

在宿主机上用 `--resolve try.kestrel.justwork.link:443:127.0.0.1` 打本机：`/login` 200、
`/assets/main.css` 200、`/admin` 302 → `https://try.kestrel.justwork.link/login`、`/` 302；
80 端口 301 → https。反代那三处配置（`proxy_buffering off`、独立的 `location = /api/voice`、
上游写 `172.17.0.1:8848`）一行未动，仍然通。

### 13.4 会话与数据面：线上确认（兼容性只有一个方向）

```bash
sqlite3 "file:/opt/kestrel-studio/data/kestrel-studio.db?mode=ro" \
  "select (select count(*) from users), (select count(*) from sessions), \
          (select count(*) from conversations), (select count(*) from messages), \
          (select count(*) from assets);"
```

- 库现状：2 用户（`user` / `super`）· 3 会话行 · 7 会话 · 44 消息 · 17 资产 · 2 视频任务。
- **一轮真模型流式对话经 Rust 落地**：会话 7 的用户轮 + 助手轮时间戳 13:45:53–54，
   `conversation_events` 37 行、`seq` 从 1 连续到 37、末行 `turn_done`。13:24 起线上就是
   Rust 容器（同机最后一个 Nest 镜像构建于 9-27），所以这一轮不可能出自 Nest；但具体是 13:24
   还是 13:45 那版分不出来——compose 每次重建都会移除旧容器，`docker ps -a` 只剩 13:47 那个。
  事件流仍是「先插 `seq` 再扇出」，与 Nest 期写的旧行同表同形状。
- `sessions` 当天新增的 3 行形状是 `{"cookie":{"expires":…,"originalMaxAge":604800000},"user":2}`
  —— principal 在**顶层 `user`，值是裸 id**，这是 Rust 侧写的。Nest 侧走 passport，
  `serializeUser` 只把 id 交出去，所以它的行是 `{"cookie":{…},"passport":{"user":<id>}}`，
  principal 嵌一层。Rust 的两个位置都读（`auth/session.rs`），Nest 读不到 Rust 的顶层 `user`
  ——**回滚方向不兼容**，这一点在回滚前要先确认。`expire` 列存 ISO-8601 文本
  （与 better-sqlite3-session-store 一致），Rust 读写两侧都认（architecture.md §13.2）。
- **切流会登出所有人**：两边都用 `HMAC-SHA256` + base64url-no-pad，但**被签的字符串不同**——
  express-session（`cookie-signature`）签的是**不含** `s:` 前缀的 token，本服务签的是
  `"s:" + token`（`auth/session.rs::sign`）。同一个 token 在两侧算出的摘要必然不同，所以 Nest 期的
  旧 cookie 在本服务上验不过，属**被拒绝**而非**被误信**。线上表现与之一致：13:24 之后表里出现的
  3 行全部是重新登录写进来的，没有一条旧会话延续的痕迹。这是「代码事实 + 表现一致」，
  不是一条抓到的拒绝日志——本服务不把 cookie 验签失败写进日志。

### 13.5 仍未覆盖（别当已验证）

1. **出图与视频任务**：`generation_tasks` 最新两行仍是 2026-09-22 的，Rust 的任务子系统
   在新链路上一次没跑过（后台轮询、成功即镜像、追加消息并推送这三步都还是 Nest 期的证据）。
2. **语音**：真拨号、101 接管、`/api/voice` 经反代的 upgrade——§12.2 那条「浏览器里一次
   都没跑过」现在仍然成立。
3. **`/admin` 的内容**：只验到守卫跳转，账号列表 / 会话只读回放没进去（需登录态）。
4. **SSE 断线续传**：本轮只验到「一轮跑完且事件连续落库」，没验到「断开后带 `last-event-id`
   重放」。§8 那份实测是 Nest 期做的。
5. **浏览器控制台零噪音**：切换后没有任何一次浏览器侧观察。

2–5 都需要登录态或真人操作。当天决定不补这轮走查，因此这五条一直是「未过」而不是「待补」，
分类与操作顺序见 §13.6 末。
同日复核见 §13.6：其中 1、2 已**正证未在线上跑过**（按时间戳查新行，语音另查 `voice session opened`
日志与「语音会话」行）；3、4、5 从服务器侧**取不到证**（Rust 无请求级日志，只读操作不写行），
只能由操作者报告，并与机器证据分开标注。

### 13.6 复核：13:47 那次部署之后，线上写入为 0（同日）

操作者报告"已在浏览器中验证过"之后先复核，而不是把结论抄进证据文件。核对方式是按时间戳查
新行（13:47:00 CST = `1793598420000` ms）：

```bash
ssh lavo-test 'sqlite3 "file:/opt/kestrel-studio/data/kestrel-studio.db?mode=ro" \
  "select (select count(*) from messages            where created_at > 1793598420000) msgs, \
          (select count(*) from assets              where created_at > 1793598420000) assets, \
          (select count(*) from conversation_events where created_at > 1793598420000) evs, \
          (select count(*) from generation_tasks    where created_at > 1793598420000) tasks;"'
```

结果 `0|0|0|0`。`sessions` 仍是 13:35 / 13:39 / 13:42 那 3 行，`docker logs --since
2026-11-02T13:47:00` 只有启动那 3 行，`conversations` 最新仍是 13:45:53 的会话 7。

**这五条要分成两类，不能混为一谈：**

| 项 | 服务器侧能否取证 | 复核结论 |
|---|---|---|
| 出图 / 视频任务 | 能——任一轮都写 `messages` + `assets`（视频另写 `generation_tasks`） | **正证未在线上跑过**（零新行） |
| 语音 | 能——`voice/session.rs` 建会话写 `title='语音会话'`，并打 `tracing::info!("voice session opened")` | **正证未在线上跑过**：库里那 3 条「语音会话」（id 4 / 5 / 6）全部写于 2026-09-22，就是 §4.1 那批 Nest 期实测留下的；11-02 一条也没有，日志里也没有 `voice session opened` |
| `/admin` 只读回放 | 当时不能——读操作不写任何行，且服务端没有请求日志 | 既不能证实也不能证伪。**当日已补开关**（§13.7），下一次走查可取证 |
| SSE 断线续传 | 同上 | 同上；补完后 `/stream?from=N` 直接可见 |
| 浏览器控制台零噪音 | 不能——发生在浏览器里，服务器永远看不到 | 仍只能由操作者报告，并标注为「操作者观察」 |

当时取不到证的根因在代码里，不是这次没查到：`main.rs` 的 `EnvFilter` 默认 `info`，而全仓没有
任何请求级日志层，`info` 只打在启动、会话清理与语音生命周期上。所以线上一次纯读的走查（打开
页面、翻历史、进 `/admin` 看回放、断线重连）在服务器侧必然无痕。**这一条已在同日改掉，见 §13.7；
本节那三次只读走查发生在改掉之前，仍然是无法佐证的操作者陈述。**

因此 P1 的剩余判据天然是两种证据拼起来的。**2026-11-02 决定：这一轮走查不做，上面五条判据维持
"未过"，不再反复提议去做。** 下面这份清单因此不是待办，而是"哪天要做就照它执行"的操作顺序：

1. **写路径**：发一张图 → 跑一个视频 → 打一次语音。每条之后本节上面的 SQL 应当出现新行，
   语音还应多一条 `voice session opened` 与一个新会话行。
2. **只读路径**：`/admin` 列表与只读回放、刷新后的事件重放、控制台条目数——这三条只能由人报告，
   报告本身记为「操作者观察」，与机器证据分开写，不混标"实测通过"。

跳过的后果写在 roadmap.md §4.4 的 P1 行：P1 停在「部分通过」，**P4（删 Node 栈）因此继续禁止**，
`nest/` 那两棵树作为唯一的回滚底牌原样留着。取证手段本身已经在线上（§13.7 的 DEBUG 开关与
§2.1 的读法），真供应商那条链路哪天出问题，第 1、2 条就是第一手排查动作——跳过验证不等于
放弃可观测性，只是不再为没有排期的走查排队。

### 13.7 访问日志落地（2026-11-02，本机 + 线上均已实测）

§13.6 那条盲区当天补掉了。实现是库的默认形态，不是自研件：`rust/server/src/http/router.rs`
挂 `tower_http::trace::TraceLayer::new_for_http()`，级别用 tower-http 自带的默认
（span / on_request / on_response = **DEBUG**，on_failure = ERROR），开关是 compose 里的
`RUST_LOG=${RUST_LOG:-info}`（为什么放 compose 不放 `.env`：见 deployment.md §2.1）。

本机验证方式与结果——不启动服务，用 `oneshot` 驱动同一个 router：

```
$ CARGO_TARGET_DIR=/tmp/kestrel-studio-target cargo test --manifest-path rust/server/Cargo.toml
  test result: ok. 164 passed; 0 failed; 2 ignored
```

新增两条测试，各钉住一个方向：

- `http::tests::the_access_log_records_the_request_and_stays_out_of_the_default_filter`：
  装一个捕获用的 layer，同一请求分别在 `info`（= 生产默认）与 `debug` 下走一遍。前者断言没有
  任何 `tower_http` 输出，后者断言出现 `method=GET`、`uri=/login?probe=1`、`status=200`，且级别
  是 DEBUG。**查询串留在日志里**这一条是重点：SSE 续传在服务器上第一次变成可见的
  `/api/v1/conversations/{id}/stream?from=N`。
- `http::tests::the_voice_upgrade_route_stays_out_of_the_access_log`：`/api/voice` 排在 `.layer()`
  **之后**（axum 只包裹注册前的路由），所以它不进访问日志。这条排除是刻意的——那是唯一劫持
  连接的路径，而它的 `hyper::upgrade::on` 接管从未上过机器，不在它前面放会碰 response body 的
  东西。测试用一次无握手头的 GET（返回 426，不触网）断言该路径零输出，防止以后被"顺手统一"。

**仍然没有的运行时证据**（别把上面的通过读成这些已过；当天已决定不为其排队，见 §13.6 末）：

1. 语音升级在有/无这层中间件下的实际行为，仍待一次真机通话。
2. 需要登录态的只读项（`/admin` 内容、带 session 的 SSE 重连）仍然只有操作者能验；
   日志现在能给他们佐证，但本节下面那次 curl 是无凭据的，只验到形状与排除项。

**同日 16:18 上线后的线上实测**（备份 `data/backup-20261102-pre-accesslog.db`，`integrity_check` ok）：

| 验的东西 | 结果 |
|---|---|
| 新二进制真的在跑 | 镜像 16:18:48 构建，`.build/linux-x86_64/kestrel-studio` 9,758,336 B，容器 `Up` |
| 默认级别确实静默 | 容器 env `RUST_LOG=info`，打了 `/login`、`/admin` 等请求之后，去色的日志里 `method=` / `status=` 计数 **0**，日志仍只有启动那 3 行 INFO |
| 打开后的输出形状 | `DEBUG request{method=GET uri=/login?probe=1 version=HTTP/1.1}: tower_http::trace::on_response: finished processing request latency=0 ms status=200` |
| 查询串保留（SSE 续传可见的设计目的） | `GET /api/v1/conversations/7/stream?from=9` 无凭据返回 401，日志里的 `uri=` 完整带 `?from=9` |
| `/api/voice` 的排除生效 | 无握手头的 GET 返回 426，日志里 `uri=/api/voice` 计数 **0**；同批 `/login`、`/assets/main.css`、`/admin`、`/stream` 都有行 |
| 同侧重部署不登人 | 容器连续重建三次（info→debug→info），`sessions` 仍是 §13.4 那 3 行，`expired sessions cleared=0`。与 §13.4 换侧登出是两回事，这里签名口径与 `SESSION_SECRET` 都没动 |

复现命令与读日志的限制在 deployment.md §2.1；那里原本写的 grep 命令是错的，已按 §13.8 改正。
### 13.8 一个踩到的坑：日志带 ANSI，朴素 grep 抓不到（2026-11-02）

`tracing_subscriber::fmt()` 开着 `ansi` 特性就**恒定**输出转义序列，不看 stderr 是不是 TTY——
Docker 收日志不是 TTY，色码照旧写进日志文件。于是字段被拆开：

```
...: \e[3mmethod\e[0m=\e[3mGET   # 原始字节里 "method" 与 "=" 之间夹着 \e[0m
```

我第一次取证用 `grep -E "method=|status="`，结果 **0 命中**，看起来像"日志没生效"，其实是被
转义序列挡住了。当时能用的写法是先去色（今天只有读 17:29 之前的旧日志才需要它）：

```bash
docker logs --since 2m kestrel-studio 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "method=|status="
```

**根治已做并上线**（提交 `704b9af`，当天 17:29 部署）：`main.rs` 按 `std::io::stderr().is_terminal()`
决定 `.with_ansi(...)`。Docker 收日志不是 TTY，所以容器里现在是纯文本；开发者在前台跑
`make run-rust` 仍保留颜色。线上复测：`docker logs | grep -E 'method=GET|status='` **不去色直接命中**
四行（`/login?probe=1` 与 `/admin` 的 on_request / on_response），同批日志里 ESC 序列计数为 **0**。
两条测试钉在 `rust/server/src/main.rs::tests`：
`a_non_terminal_log_is_plain_text_so_access_lines_are_greppable`（纯文本下 `method=GET`、
`uri=/login?probe=1` 连着写且无 ESC）与 `ansi_mode_is_what_broke_the_ops_grep`（开着 ANSI 时确有
ESC 且 `method=GET` 不再连续）——后者直接复现根因，不是转述实现。`cargo test` 166 passed / 0 failed。

**唯一残留**：17:29 之前已经写进日志文件的那些行仍带 ANSI，读那段历史日志要去色；新行不需要。

### 13.9 WASM 包体积复测（2026-11-02 18:16，含对话页两项）

对话页的下载对话框与历史刷新是第一批**真进浏览器**的 DOM 代码之外还带进了 canvas 与 fetch 路径的
功能，`make build-wasm` 之后重新量一次（`wasm-opt` 已跑，profile = release，`panic = "abort"` +
`codegen-units = 1` 保持）：

```bash
for f in public/wasm/kestrel_client_bg.wasm public/wasm/kestrel_client.js \
         public/assets/main.js public/assets/main.css public/assets/voice.js public/capture.js; do
  printf "%-38s raw=%8d gz=%7d\n" "$f" "$(wc -c < "$f")" "$(gzip -c9 "$f" | wc -c)"
done
```

| 文件 | raw | gz |
|---|---|---|
| `public/wasm/kestrel_client_bg.wasm` | 368,270 | **131,647** |
| `public/wasm/kestrel_client.js`（glue） | 55,333 | 9,577 |
| `public/capture.js`（worklet，删不掉的那 20 行 JS） | 1,687 | 905 |
| `public/assets/main.js`（现客户端） | 60,503 | 18,195 |
| `public/assets/main.css` | 16,153 | 3,527 |
| `public/assets/voice.js` | 6,517 | 3,064 |

读法：

- **对话页**从 21.7 KB gz（`main.js` + `main.css`）换成 wasm 一侧要付 142.1 KB gz
  （wasm + glue），约 **6.5 倍**；§12 当时那句「约 85 KB gz」已经低了 46 KB，因为语音之后又加上了
  对话页的 canvas 重编码与列表刷新。
- 全站两个页面都换过去则是 142.1 + 0.9 KB gz 对 24.8 KB gz（`main.js` + `main.css` + `voice.js`）。
- 这个数字**不作为决策输入**（§12 那句判断仍然成立），这里只负责让 roadmap.md §4.1 不再挂着
  一个过期的「实测持平」。
  **本节只量总体积，逐项归因在 §14**（次日用 `/tmp` 副本重编做的，没装任何新工具）：两个页面的
  DOM 层合计 110.5 KB gz，占这 131.7 KB gz 的 84%；markdown 子集 5.6 KB；今天新加的 canvas 重编码 1.0 KB。
  当时留的那句「先量一次再决定」已经做掉了，glue 那 9.6 KB gz 仍是 wasm-bindgen 按导出函数生成的桥。
- 产物本身仍未被任何模板引用（`initChat` / `initVoice` 零调用点），所以这些字节目前**一个都没
  下载给用户**。切换发生在 P4 之前那次独立改动。

## 14. WASM 包的逐项归因（2026-11-03，闭掉 roadmap.md §4.1.1 的第二个开放项）

方法沿用 §11.5 的做法：**在 `/tmp` 的副本上重编，仓库一字不改**（`cp -R rust/client crates /tmp/kestrel-size/`，
保持 `../../crates/*` 的相对层级），每测一项就 `rm -rf src && cp -R src.base src` 还原。工具链与
`make build-wasm` 完全一致（`wasm-pack build --release --target web`，`wasm-opt` 已跑，
`opt-level = "s"` + `lto` + `panic = "abort"`）。基线复测 131,655 B gz，与 §13.9 的 131,647 B gz
差 8 B（副本路径长度不同），可视为同一测量。

做法是**减法归因**：把某个模块的 `pub mod` 声明或调用点去掉后重编，差额即该模块被引用时才付出的代价。
两项 DOM 层用同一条链相减，因此彼此可加；markdown 与 canvas 那两项是在完整基线上单独测的，与前者有重叠。

| 变体 | 相对基线去掉的东西 | gz | 该块贡献 |
|---|---|---|---|
| 基线 | — | **131,655** | |
| F | 仅去掉 `chat_page` | 77,663 | **对话页 DOM 层 53,992（54.0 KB）** |
| H | 再去掉 `wire` / `audio` / `voice_page` | 21,182 | **语音 DOM+音频+socket 层 56,481（56.5 KB）** |
| E | 只留 `buildInfo` 一个导出 | 7,724 | **地板 7,724（7.7 KB）**：wasm-bindgen 运行时 + Rust 核心 + `js_sys`。对照 H 的 21,182 可知**共享底**（proto 的 serde 派生 + markdown + 两个 core）合计 **21.2 KB** |
| A | 把三处 `kestrel_md::markdown` 换成 `escape` | 125,906 | **markdown 子集解析器 5,749（5.6 KB）** |
| C | `render_data_url` 直接返回 Err（canvas 路径不可达） | 130,603 | **canvas 重编码 1,052（1.0 KB）** |
| G | 去掉 `chat_page` 再去掉 `chat_core` | 77,663 | **0**：没有调用方时纯判定层整块被链接器丢掉 |

四条读法：

1. **DOM 层的真实体积这个问题现在有答案了**：两个页面的 DOM 层合计 **110.5 KB gz**，占整包
   131.7 KB gz 的 **84%**；共享底（含 markdown 与解析层）只占 16%。对照现客户端整站 24.8 KB gz，
   启用 WASM 后的载荷是它的 **5.3 倍**。§4.1.1 那条「未测」的开放项就此关闭；当初设想的那条退路是否用得上，见下面第 4 点。
2. **§11.5 当年那面墙已经不是墙**：markdown 从 `pulldown-cmark` 的约 74 KB 换成共享子集后是
   **5.6 KB**，与地板（7.7 KB）同量级。当年为它做的取舍是对的，不必再回头。
3. **昨天新增的功能几乎不收钱**：下载对话框（canvas + 重编码 + 文件名与体积计算，2026-11-02 落地）实测 **1.0 KB gz**，
   占整包 0.8%。而"纯判定层 + 宿主测试"这条路在体积上没有代价（G 行为 0），所以 §4.1 说的那笔
   真实代价——"客户端从此没有类型检查与 jsdom 之外的运行时护栏"——不需要用体积去换，两者不是交易。
4. **真正贵的是 DOM 与音频，各约 55 KB**，两者结构同类（web-sys 绑定 + 事件 wiring + 各自的编解码
   与 JSON 路径 —— 这一句是结构描述，不是逐项测出来的；本次测到的是总量与两块的差）。§4.1.1 当时写的退路"服务端吐 HTML、客户端不做 markdown"**不会命中这块**：
   省下 markdown 的 5.6 KB 也还是 126 KB。真要降，抓手在 110.5 KB 那两个 DOM 层里，不在渲染器上。

复现：`make build-wasm` 的量法见 §13.9；变体法只动了副本的 `src/lib.rs` 的 `pub mod` 声明与
`render_data_url` / 三处 markdown 调用点，一轮测量 40 秒。**本次未装任何新工具**（没有 `cargo-bloat`）。

## 15. 助手到底用不用表格：对已存消息的观察（2026-11-03）

roadmap.md §4.1.1 第一条开放项的两个选法里，有一个是"接受子集现状，并观察助手是否真用表格"。
观察部分现在做完了，数据在下面；**决定仍然没做**，这一节只交证据。

口径：把两个库里全部 `role='assistant'` 的消息取出来做构造统计，只读，`mode=ro`。行首类构造一律用
"换行 + 标记"判定，避免把正文中间的 `-` 当成列表：

```sql
select count(*) from messages where role='assistant';                                        -- 分母
select sum(instr(content,'|---')>0 or instr(content,'---|')>0 or instr(content,'| :--')>0)   -- 表格分隔线
  from messages where role='assistant';
select sum(instr(content,'~~')>0) from messages where role='assistant';                       -- 删除线
select sum(instr(char(10)||content, char(10)||'- ')>0
    or instr(char(10)||content, char(10)||'* ')>0
  from messages where role='assistant';                                                       -- 行首列表
-- 其余同理：char(10)||'#' 标题、char(10)||'>' 引用、'**' 粗体、'](' 链接、char(96) 反引号
```

| 构造 | 本地 26 条 | 线上 23 条 | 合计 49 条 | `crates/md` 子集 |
|---|---|---|---|---|
| 表格分隔线 | 0 | 0 | **0** | **不支持** |
| 删除线 `~~` | 0 | 0 | **0** | **不支持** |
| 行首列表 | 1 | 13 | **14** | 支持（`-` / `*` / `1.`） |
| 粗体 `**` | 1 | 1 | 2 | 支持 |
| 行首标题 `#` | 0 | 0 | 0 | 支持 |
| 围栏代码块 | 0 | 0 | 0 | 支持 |
| 行内代码 | 0 | 0 | 0 | 支持 |
| 链接 `[](…)` | 0 | 0 | 0 | 支持 |
| 行首引用 `>` | 0 | 0 | 0 | 支持 |

样本构成（决定这些数据能推到多远）：

- 合计 **49 条助手消息**，时间跨度 2026-09-20 → 11-02；本地按模式 `(无)10 / chat 7 / video 6 / auto 2 / image 1`，
  线上 `(无)10 / auto 11 / video 2`。
- 长度很短：本地平均 89 字符、最长 336；线上最长 799。
- 14 次列表命中的构成（线上 13 条 + 本地 1 条；11 条按"含比如/告诉/可以帮你或长度 <400"归类并抽 2 条通读，其余 4 条逐字看过）：**11 条**是自我介绍式引导列表
  （"比如：- 一只戴宇航头盔的柴犬 / - 一张海报…"），**2 条**是设计说明型长回答里排的清单
  （logo 的配色/尺寸理由），**1 条**在本地。里面**没有一条**需要表格或对齐列。
- 也就是说：**这个语料回答的是"这个产品的助手会不会写表格"，不是"这些模型有没有能力写表格"**。
  换模型、或把用法转向长文（`chat` 模式写报告、列对比表），这份观察就不再适用——重跑上面那段 SQL 即可，
  两个库都是只读的。

对那条开放项的意义（不含结论）：子集**缺的两种语法命中率为 0**，而助手**真正用到的构造子集全都支持**。
所以"给 `crates/md` 补表格/删除线"当前的收益是 0 次观察到的需求。

**一处更正**：本节初稿把代价写成 §4.1.1 原话里的"两侧同改义务"，那已经不成立——11-02 之后 SSR 与浏览器
编的是同一个 `kestrel-md`（`rust/server/src/view/html.rs` 只剩再导出），实现点只有一个，补语法不存在
重新制造口径分裂的风险（口径分裂正是 `e7b2911` 那次的原因，现在结构上已经关掉了）。
**决定已做（同日）：补。** 代价不是估计了——实现与复测在 §16。本节初稿留的那句"要不要仍然补是你的
判断"到此作废。

### 15.1 第二次普查（同日，补上表格之后；哨兵已挂成命令）

补完表格再数一次，顺手把"还不支持的那几类"一起数了。入口已固化：
`make census`（本地库）与 `make census-prod`（线上），脚本 `scripts/markdown-census.sql`，
一律 `mode=ro` 打开。SQL 只用 `LIKE` 与 `char()`——线上宿主机的 sqlite 是 3.7，没有 CTE 也没有
`instr()`，第一版就是在那儿报的 syntax error。

| 构造 | 本地 26 条 | 线上 23 条 | 子集 |
|---|---|---|---|
| 原始 HTML（`<div`/`<p `/`<br`/`<table`/`<img`） | 0 | 0 | 一律转义成源码（刻意的安全性质） |
| 图片 `![](` | 0 | 0 | 不支持（图片走资产通道，不经 markdown） |
| 任务列表 `- [ ]` | 0 | 0 | 不支持（`marked` 会画成勾选框） |
| 波浪线围栏 `~~~` | 0 | 0 | 只认 ```` ``` ```` |
| 脚注 `[^` | 0 | 0 | 不支持 |
| setext 标题（上一行 + `===`） | 0 | 0 | 只认 ATX |
| GFM 表格 | 0 | 0 | **今天起支持** |
| 删除线 | 0 | 0 | **今天起支持** |
| 行首列表 | 1 | 13 | 支持 |
| 粗体 | 1 | 1 | 支持 |
| 链接 / 代码围栏 / 行内代码 | 0 | 0 | 支持 |
| 最长一条（字符） | 336 | 799 | |

三条读法：

1. **表格补完之后，剩下不支持的六类命中仍然是 0**。所以 §4.1.1 关掉之后不需要再开新的口子：
   现在没有观察到的需求，只有"哪天模型改写法"的假设。
2. 因此哨兵的作用从"决定补不补"变成"**什么时候该重开这个决定**"：任意一行不支持的构造变成非 0，
   就该照 §16 的流程走一遍（代价量一次、断言补上、体积复测）。命令已经在那儿，不用重新想口径。
**补记（同日 11:00 后）**：这张表里"表格 0 命中"的结论**当天就被推翻了**——灰度拨到 WASM 之后，
会话 8 的助手回答（511 字符）里就有 `|---` 型表格分隔线，即第 1 次真实命中（上一行的 0 是
11:03 之前的 49 条样本，仍然成立，只是样本太小）。这直接说明 §16 那个"补"的决定是对的：
再等一次观察的话，第一次写表格的用户看到的就是裸竖线。

3. "原始 HTML"那行是**启发式关键字列表**，不是完备检测；而且它的失败模式是"显示成源码"，
   不是执行——子集之外一律转义这条性质由 §16 的断言钉着，不依赖这行数字。


## 16. 子集补上 GFM 表格与删除线（2026-11-03，实现 + 复测）

§15 的观察是"0 次命中"，仍然补了，理由只有一条：线上跑的是子集这一侧，而这个差异**已经生效**，
留着它等于留一颗没人盯的雷；补的代价测出来只有 2 KB 量级（见下）。决定与推荐过程在 §15 与
roadmap.md §4.1.1。

实现：`crates/md/src/lib.rs` **+153 行**（含注释，纯代码约 140 行）。Rust 这一侧不存在"记得同步"
的问题——SSR 与浏览器编的就是同一个 crate；`nest/` 那两份仍走 `marked`（P4 前保留），而 marked
本来就支持表格，所以这次改动是把口径差**缩小**而不是拉开。断言加在
`rust/server/src/view/tests.rs`：原有条目 +2，另加一整条
`the_table_rules_that_keep_prose_from_turning_into_a_table`（13 条断言，全部钉行为而非注释）。

体积复测（`wasm-pack build --release --target web`，同一 profile）：

| | raw | gz |
|---|---|---|
| 补之前（§13.9） | 368,270 | 131,647 |
| 补之后 | 373,426 | **133,734** |
| 差 | +5,156 | **+2,087（+1.6%）** |

四条与 `marked` 不同的刻意边界（都写在 helper 注释里，也都有断言钉着）：

1. **单列表格必须两侧围栏**（`| a |` 压在 `| - |` 上才算）。这是 GFM 自己的消歧规则；
   不收这条，`2 | 3` 这种正文就会开出一张表。
2. **`\|` 是单元格内竖线的唯一写法**。分格在 inline 之前跑，看不见代码串，所以
   `` `a | b` `` 会照样被拆成两格、反引号留成字面文本——参考实现同样如此，这不是缺陷而是同一条取舍。
3. **表头定列数**：多出的单元格丢弃，缺的补空 `<td></td>`。凭空造一列没有对齐规则可用。
4. **分隔行是唯一入场券**：没有 `---` 行，正文里的竖线一律还是段落
   （`名字 | 分数` 两行软换行仍是一个 `<p>`，与子集既有段落口径一致）。

安全性质没动：单元格内容照样过 `inline()`，表格里的 `<script>` 仍是
`&lt;script&gt;`——这条有断言，因为"子集之外一律转义"正是它相对 `marked` 唯一的优势。

验证：`cargo test`（rust/server）**167 passed / 0 failed / 2 ignored**（新增 1 条测试）；
`cargo check --manifest-path crates/md/Cargo.toml` 与 `wasm-pack build` 均 0 warning。

**当天 10:18 已部署**（先 `sqlite3 .backup` 出 `data/backup-20261103-pre-tables.db`，integrity ok）：
镜像 10:18:07、二进制 9,766,128 B（比上一版 +7,792 B，与 md 增量同量级）、
`public/wasm/kestrel_client_bg.wasm` 373,426 B —— 与上面那张表的 raw 一致，说明线上跑的就是本次构建的产物。
健康复测：`/login` 200、`/assets/main.css` 200、`/wasm/…` 200、`/admin` 302、公网 HTTPS 200；
`sessions` 仍是那 3 行、`expired sessions cleared=0`，**同侧部署没有登出任何人**（§13.4 的换侧才会计）；
启动日志三行、无 error/warn，且日志是纯文本（§13.8 的修复在线上生效）。

**仍未观察到的**：表格里那个 `<table>` 在**线上页面上**的样子——SSR 的会话页需要登录态，我没有凭据，
也不该去拿。渲染逻辑有 13 条断言钉着、部署产物也确认是新的，但"在浏览器里看到一张渲染出来的表"
这一步仍属于操作者观察。
## 17. 浏览器包切换开关（2026-11-03，已实现；线上档位随 §20 变动，当前 `wasm`）

`CLIENT_BUNDLE` 决定 `/` 与 `/voice` 加载哪份客户端：`nest`（线上现状）或 `wasm`（P3 那份）。
运维口径与回滚在 deployment.md §2.2，这里只记实现与证据边界。

代码面：`config::ClientBundle`（未知值**启动失败**，不静默回退——与 `BAILIAN_REASONING_EFFORT`
的静默回退是有意不同的，理由写在函数注释里）；`ChatPage.client` 与 `voice_page(.., client)` 把它
带进模板；`client_script()` 一个函数决定两个页面加载什么，WASM 分支用的是内联 module loader
（`WASM_LOADER`），**不新增一个 JS 文件**：JS 与 `.wasm` 是两份产物，落成文件就会有缓存错位的
可能，内联就没有"半个客户端"这件事。加载失败会在页面上落一条 `.client-error`（样式加在共享的
`nest/client/styles/main.css`，P4 迁样式时必须一起搬）并 `console.error`。

本机证据：

- `cargo test`（rust/server）**169 passed / 0 failed**，其中新增两条：
  `the_client_switch_hands_each_page_exactly_one_client`（两页各自只带一个客户端、两个页面引用的
  模块路径必须一致、`/capture.js` 那份 JS 地板仍按 `audio.rs` 写的路径存在）与
  `the_client_bundle_round_trips_and_refuses_nonsense`（compose 写的字符串能解析回来，其余一律拒）。
- `make build-wasm` 产物未变（这次没动客户端代码）；`rust/client` 侧 `cargo test` 36 passed，
  `cargo check --target wasm32-unknown-unknown` 0 warning。
- 默认值 `nest` 使渲染结果与切换前**逐字节相同**——原来那条断言 `http::tests` 里的
  `<script type="module" src="/assets/main.js">` 仍然是绿的，这就是"默认不改行为"的证明。

宿主机上的复测（当天 10:54 部署 `nest` 档，随后各拨一次）：

| 检查 | 结果 |
|---|---|
| 部署后默认档 | `image=10:54:34`、二进制 9,770,096 B、compose 注入 `CLIENT_BUNDLE=nest`、启动日志 `client="nest"`；`/login` 200、`/assets/main.css` 200、`/admin` 302、公网 HTTPS 200 |
| 拨到 wasm | `CLIENT_BUNDLE=wasm docker compose up -d` → `client="wasm"`、`/login` 200 |
| 拨回 | 同一条命令去掉变量 → `client="nest"`、`/login` 200、`/admin` 302 |
| 会话 | 三次重建（部署 + 两向拨档）之后 `sessions` 仍是 3 行、`expired sessions cleared=0` —— **换客户端不登人**，这条现在是实测而不是推断 |
| 顺带 | 日志已是纯文本（`client="nest"` 可直接 grep），§13.8 那个修复在线上确认 |
| 10:57 拨到灰度 | `CLIENT_BUNDLE=wasm RUST_LOG=debug docker compose up -d` → 启动日志 `client="wasm"`；`/wasm/kestrel_client.js` 与 `/wasm/kestrel_client_bg.wasm` 都 200，且**各留下一条访问日志**（`method=GET uri=/wasm/… status=200`）——取证通道确认通了；公网 `/login` 仍 200，`sessions` 仍 3 行 |

**没有的证据**（别把上面的通过当成这些已过）：

1. `initChat` 在真实浏览器里的行为——11:00 到 13:08 这段灰度里发生了一次，报回两条 bug，取证与
   处理见 §19；`initVoice` 至今一次都没有（那段窗口里 `/api/voice` 零请求）。
2. `client=wasm` 传到位之后**页面本身**长什么样：会话页要登录态，我没有凭据。宿主机上能确证到
   "这个值真的进了进程"（上面那张拨档表），再往下——loader 是否执行、`initChat` 画得对不对——
   需要一次带登录态的浏览器观察。
3. 是否真有一张 `<table>` 渲染出来（§16 的语法补齐后同样还没在页面上看过）。

要补第 1、2、3 条：服务器上 `CLIENT_BUNDLE=wasm docker compose up -d`，浏览器开 `/`，看控制台与
一次真实轮次；同时在 DEBUG 访问日志里应当看到 `/wasm/kestrel_client.js` 与 `/wasm/kestrel_client_bg.wasm`
两条 GET（§13.7 的开关）。出问题就同一条命令去掉变量回滚——服务端、session、cookie 全都不动。

## 18. WASM 客户端的第一次真实浏览器加载（2026-11-03 11:00 之后，日志取证）

灰度拨到 `wasm` 之后，宿主机上的 DEBUG 访问日志 + 库里新行给出了机器证据。窗口取
`docker logs --since 2026-11-03T11:00:00`，请求行的提取方式（每响应一行）：

```bash
docker logs --since 2026-11-03T11:00:00 kestrel-studio 2>&1 \
  | sed -n 's/.*method=\([A-Z]*\) uri=\([^ ]*\).*status=\([0-9]*\).*/\3 \1 \2/p' \
  | sort | uniq -c | sort -rn
```

结果（1807 行日志，其中含 sqlx 的 DEBUG 语句行）：

| 观测 | 数字 | 含义 |
|---|---|---|
| `GET /wasm/kestrel_client.js` | **2 × 200 + 13 × 304** | 浏览器真的拉到了 glue；13 次 304 说明后续加载走的是协商缓存 |
| `GET /wasm/kestrel_client_bg.wasm` | 2 × 200 + 13 × 304（340,600 B 那份） | 同上，wasm 本体 |
| `POST /api/v1/conversations` | 1 × 201 | WASM 客户端建了会话 8 |
| `GET /api/v1/conversations` | **5 × 200** | 今天新加的**历史下拉刷新**在真实页面上跑了 |
| `POST /api/v1/conversations/8/turns` | 1 × 202 | 提交轮次走的是分离后的提交端点 |
| `GET /api/v1/conversations/8/stream?from=0` | 1 × 200 | EventSource 由 WASM 打开，且 `?from=` 在日志里可读 |
| `GET /?c=8`、`/`、`/?`、`/?c=5..7` | 若干 × 200 | SSR 首屏与历史切换 |
| `GET /api/v1/assets/{7,8,9,10}/download` | 6 × 200 | 旧会话的资产在页面上取到了 |
| 非 2xx/3xx | **0 条** | 没有失败请求 |
| `WARN` / `ERROR` / `panic` | **0 条** | 服务端侧零异常 |
| `/api/voice`、`generation_tasks`、`assets` 新行 | 0 | **语音与出图/视频这次没试** |

数据面：窗口内 `messages` +2（用户 8 字符 / 助手 511 字符，`mode=auto`），
`conversation_events` +179，`seq` 1..179 连续且**以 `turn_done` 收尾**（1 reasoning 65、text 111），
新会话 8 建档。也就是说 WASM 客户端把"建会话 → 提交 → 订阅 → 逐帧绘制 → 正常收尾"这条主链
在真实浏览器里走通了一次，没有一次失败请求。

仍然没有的：

1. **浏览器控制台是否零噪音**——日志看不见浏览器。这一条仍只能是操作者观察。
2. 语音通路（`/api/voice` 一次都没请求过）。
3. 出图 / 视频任务（零 `assets`、零 `generation_tasks` 新行），以及**图片预览与今天刚接上的下载对话框**。
4. 表格这次确实出现了（见 §15.1 补记），但"渲染成了表格还是裸竖线"要靠眼睛或截图，日志不区分。

## 19. 第一次灰度报回的三条：两条 WASM 回归、一条既有规则（2026-11-03）

灰度拨到 `wasm` 后操作者在页面上试过，报回三条。逐条对代码与日志核过，结论并不一样：

| 报回 | 结论 | 依据 |
|---|---|---|
| 1. 新会话点了没反应 | **WASM 侧回归**（竞态） | 旧实现是 `location.search = ""` 紧跟 `location.reload()`：浏览器会中止排队中的前一次导航，于是**重载的还是带 `?c=<id>` 的当前页**。日志里两种结果混着出现——6 次 `GET /?c=8`、3 次 `GET /?`——正是竞态的形状，不是恒发失效。Nest 版没这个写法（它在客户端就地清舞台），所以这条只属于 WASM 那份 |
| 2. Enter 该发送却只换行 | **WASM 侧缺失** | `chat_page.rs` 从来没绑 `keydown`：`bind()` 里只有 `send` 按钮的 click，注释却写着「submit button and Enter are wired」。Nest 版 `main.ts:967` 是 `Enter && !shiftKey && !isComposing → requestSubmit()` |
| 3. 历史会话删除按钮没了 | **不是回归，是既有规则** | 删除控件只在 `!is_super_admin(user)` 时渲染，两个服务端同一条：`nest/server/src/view/view.controller.ts:116` 与 `rust/server/src/view/controller.rs:120`；两个客户端都按 `data-can-delete-conversations` 办事（`paint_history` 用的就是它）。会话 8 的属主是线上那个**超管账号本身**（id 2，role `super`，账号名从略），所以他**在 Nest 客户端下同样没有删除按钮**。要让超管也能删是产品决定，动的是 architecture.md §12.1 那条规则，不是这个客户端 |

处理：

- 1 改成**一次**导航（`location.set_href(pathname)`），原因写进注释。这类 bug 只在真实浏览器里暴露，
  宿主机日志是唯一能反推它的证据。
- 2 补 `keydown`，判定放进 `chat_core::enter_submits(shift, composing)`（4 条断言）——**IME 守卫是要点**：
  没它的话，中文拼音选词的 Enter 会把半截 prompt 发出去。`rust/client/Cargo.toml` 因此加 `KeyboardEvent`。
- 3 不动代码。若要把超管纳入可删，需要两个服务端一起改，那是 §12.1 的事。

**2026-11-06 更正第 1 条与第 3 条**（浏览器实测，见 §22）：

- 第 1 条的修复**没修到位**。`location.set_href(pathname)` 确实消除了双导航竞态，但那次导航落到
  `/` 之后，服务端 `view/controller.rs` 会把无 `?c=` 的请求回落到"该用户最近的一会话"，于是舞台
  仍是上一会话——实测从 `/?c=7` 点新会话后 `data-conversation-id="8"`、2 轮旧内容原样在屏上。
  Nest 版靠 `resetToNewConversation()` 在客户端就地清舞台并删掉 `data-conversation-id`，从不走服务端。
- 第 3 条的结论只对了一半。超管没有删除按钮确实是既有规则；但 wasm 客户端读
  `dataset("can-delete-conversations")` 用的是属性的 kebab 拼法，**恒为 None**，所以普通用户在
  wasm 那一侧也同样拿不到删除控件。当时那条报回是"规则 + bug"两因叠加，不是单纯的规则。

验证边界：`cargo test`（client）37 passed、`cargo check --target wasm32-unknown-unknown` 0 warning、
`make build-wasm` 通过（133,983 B gz，比修之前 +239 B）。**修复后没有在浏览器里跑过**——13:08 已把档位
拨回 `nest`，线上目前还是那份 TS 客户端；要再验一次必须重新部署 + 再拨一次灰度。（后续：13:21 部署了
修复版、13:23 重新拨到 `wasm`，机器证据见 §20；浏览器复验在那之前仍未做。）

顺带对出来的两个 parity 缺口（**本次未修**，等确认）：`main.ts` 还有「textarea 随输入长高」（974）与
「点击下拉外部自动收起」（984），WASM 版都没做。不影响正确性，只影响手感。

## 20. 修复版上线并第二次拨到 `wasm`（2026-11-03 13:21 / 13:23，机器证据）

两步都在宿主机侧跑，所以这张表能证明的只到「线上跑的是哪份包」这一层。

| 时刻 | 动作 | 证据 |
|---|---|---|
| 13:21 | 先备份，再 `make deploy`（它不转发 `CLIENT_BUNDLE`，故按默认 `nest` 起） | 备份 `data/backup-20261103-pre-clientfix.db`（163,840 B，与 12:00 那份同尺寸）；镜像 `bca8647b23f1`；启动日志 `client="nest"`；`/login` 200、`/assets/main.css` 200（md5 `edcb8146…` 与本机 `public/assets/main.css` 相同）、`/admin` 302、`/` 302；公网 HTTPS 经 `--resolve` 打 `try.kestrel.justwork.link` 也是 200 且 md5 相同；重启后 WARN/ERROR 计数 0；`sessions` 3、`conversations` 8、`messages` 46，与部署前一致 |
| 13:23 | `CLIENT_BUNDLE=wasm RUST_LOG=debug docker compose up -d` | 容器 Recreated → `Up`；启动日志 `client="wasm"`；`/wasm/kestrel_client.js` 200 · 56,942 B · `text/javascript`，`/wasm/kestrel_client_bg.wasm` 200 · 374,373 B · `application/wasm`；两者 md5（`7f84eaa3…`、`5d59f758…`）与本机 `make build-wasm` 的产物逐字节相同 |

边界，别把这张表读成「修复已生效」：

- §19 那两条的判据在浏览器里——新会话要落到不带 `?c=` 的 `/`，Enter 要发送、Shift+Enter 要换行。
  日志只能证明页面跑的是 wasm 那份，证不了这两条。
- 容器是 `FROM scratch`，**镜像里没有 `sh`**，`docker exec` 这条路永远不通，探测只能在宿主机侧 curl；
  §13.2 那些探针因此长成那样。
- `debug` 是取证档位。只关日志保留灰度：`CLIENT_BUNDLE=wasm docker compose up -d`；两者一起退回：
  `docker compose up -d`（无需重建，也不登出任何人）。

## 21. 灰度第三天的宿主机核对（2026-11-06 上午，全程只读）

起因：11-03 13:23 拨到 `wasm` 之后没人再动过，而 roadmap.md §3 里那句「修复后的浏览器复验仍未做」
需要一次现状核对，不能一直靠引用三天前的记录过日子。证据全部来自宿主机的 `docker logs`、
`sqlite3 …?mode=ro` 和 `curl 127.0.0.1:8848`；**没有改任何状态**（未重启容器、未动档位、未写库）。

| 探针 | 结果 |
|---|---|
| 容器 | `kestrel-studio:latest` `Up 2 days`（= 11-03 13:23 那次 `up -d`，三天没有新部署）、`/login` 200、`docker stats` 内存 2.293 MiB、`0.0.0.0:8848->8848/tcp` |
| 浏览器包档位 | 启动日志 `2026-11-03T05:23:48Z INFO kestrel_studio: … listening on http://localhost:8848 client="wasm"` ——**灰度仍在** |
| 日志档位 | **`RUST_LOG=debug` 忘了收回**，还是 §20 那次 `up -d` 带进去的。json-file 驱动且 `Config={}`（无轮转无上限），日志文件 **16 MB / 2.8 天 ≈ 5.7 MB/天**；31,840 行里 **30,903 行（97%）是 sqlx 的 DEBUG 语句**（task worker 每 8s 一次查询 + 会话过期清扫每 8s 一次），访问日志只占 1,107 行。deployment.md §2.1 第 4 条「只在走查的那几分钟开着，走完关掉」这次没被执行 |
| 磁盘 | 宿主机 `/` 45 G 已用 / 99 G，可用 50 G。这个增速不急，但它没有上限 |
| 数据库 | `messages` 46、`conversation_events` 216、`assets` 17、`sessions` 3；**`created_at > '2026-11-03 13:23'` 的消息 0 行**（与 §20 部署前后那三个数一模一样） |

浏览器到底做到了哪一步——按时间轴读访问日志（11-03 之后的行已无 ANSI，字段直接命中）：

| 时刻（本地） | 证据 | 能证明什么 |
|---|---|---|
| 13:24:07、13:25:16 | `/wasm/kestrel_client.js` 与 `_bg.wasm` 各一对 200，附近没有已认证的 `GET /` | 与 §20 记录的宿主机 curl 探针时间吻合（拨档后 80 秒、188 秒） |
| 13:25:55 | 已认证的 `GET /` 200 → 同一秒内 wasm 两个文件 **200** → 13:25/13:26 各一次 `GET /api/v1/conversations` | **修复版包被真实浏览器全新下载并执行过一次**：首屏骨架、wasm 加载、历史列表接口都到了 |
| 13:40:37–13:41:10 | `GET /` 200 五次；`GET /?c=8`、`/?c=6`、`/?c=5` 若干；`GET /api/v1/conversations` 六次；wasm 全部 304 | 同一个人反复整页换会话、切历史——wasm 侧的 `refresh_history` 与按 `?c=` 的导航在线上被走到了 |
| 14:03:24 | 又一次 `GET /` 200 + wasm 304 | 还在页面上 |
| 14:03 之后到本次核对 | 只有未认证的 `GET /`→302 + `GET /login` 200，以及公网扫描（见下） | **没有人再登录过** |

**没有的东西才是结论**：自 13:23 拨档起，`method=POST uri=/api/v1/...` **0 条**、
`uri=/api/v1/conversations/{id}/stream` **0 条**、INFO 上的 `voice session opened` **0 条**。
§19 那两条修复里，「Enter 该发送」这一条从未被按下过——一整轮都没提交，出图 / 视频 / SSE 续传
照旧拿不到新链路上的证据，`/voice` 的 WASM 分支仍然一次都没跑过。P1 因此仍是**部分通过**，
P4 的闸门照旧关着。

顺带把两件不相干的事测清了：

1. **静态服务面对扫描的响应**：`/.env` 404、`/../.env` 400、`/assets/../data/kestrel-studio.db` 400
   （`http/static_files.rs` 的 `resolve_under` 拒绝 `..` 与 NUL，静态根只有 `public/`，所以仓库根那份
   `.env` 不在这个面的可达范围内）。唯一还能取到的点文件是 `/wasm/.gitignore`（200，1 字节，
   构建产物自带的忽略文件），没有信息量。
2. **扫描确实在发生**：11-04 与 11-05 两天共 116 行请求（11-03 是 346 行），路径集中在
   `/.env`、`/web/.env`、`/wp-config.php.bak`、`/wp-json/`、`/storage/logs/laravel.log`、
   `/vendor/composer/installed.json` 这一类，外加 11-04 01:57:58 一秒内的四个 POST
   （三个 `/login`、一个打在 `/` 上）。入口是 openresty 的 443——deployment.md §3 记着安全组只放行
   80/443，`8848` 虽然发布在 `0.0.0.0` 但不从公网直连。两个服务端都**没有登录限流或失败锁定**
   （`nest/server/src/auth/` 与 `rust/server/src/auth/` 都查过），这条只记录事实，动不动它是个排期决定。

边界，别把这张表读成「复验已做」：

- 「新会话」是否落到了不带 `?c=` 的 `/`——裸 `GET /` 200 有五次，但它与手工刷新在日志里同形，**分不开**。
- 控制台噪音任何时候都只在浏览器里，日志证不了。
- 13:25:55 那一簇判定为浏览器而不是 curl，依据是三条同时成立：`GET /` 返回 200（**带 session
  cookie 才会**，未认证是 302）、wasm 两个文件在同一秒内成对取走、随后紧跟
  `GET /api/v1/conversations`。宿主机侧的 curl 探针不带 cookie，也打不进这个需要登录态的接口。
- 待收的一处运维状态：debug 档位还挂着。收回且保留灰度 = `CLIENT_BUNDLE=wasm docker compose up -d`
  （重建容器，不重新构建、不登出任何人，这条性质 §20 已实测）。**本次未执行，等用户确认。**

顺带按源码复核了 §19 记下的两个 parity 缺口，**11-06 仍未修**：`nest/client/scripts/main.ts:975`
的 textarea 随输入长高（wasm 侧 `chat_page.rs` 没有 `input` 监听），以及 `main.ts:985` 的
`.menu-dropdown` 点外部收起（wasm 侧只在 `Dom::query` 里取过这个元素，没有 document 级委托；
它已有的那处 outside-click 是下载 `<dialog>` 的 backdrop，不是同一个东西）。两条都不影响正确性，
只影响手感，所以仍是「等确认」而不是待办。

## 22. WASM 客户端的第一次真浏览器回归测试（2026-11-06，chrome-devtools MCP）

载体：用户已登录的那个标签页（账号 `tww316`，超管），档位 `client="wasm"`。全部判据取自
`list_console_messages` / `list_network_requests` / `evaluate_script` 的返回值，不是肉眼印象。

### 22.1 通过

| 判据 | 证据 |
|---|---|
| 浏览器控制台零噪音 | `/`、`/?c=7`↔`/?c=8`、提交一轮、`/?c=9`、`/voice` 共 5 次导航之后，`list_console_messages` 全部返回**零条**（error / warning / issue 都没有） |
| Enter 发送 | `POST /api/v1/conversations` **201** → `POST /api/v1/conversations/9/turns` **202** → `GET /9/stream?from=0` **200**，回复渲染到页面。§19 那条"从未被按下"的判据到此取到 |
| Shift+Enter 换行 | 按下后 `#prompt` 的值为 `"回归测试第一行\n"`（换行 1 个），网络面板**没有任何 POST** |
| IME 守卫 | 合成 `KeyboardEvent('keydown', {isComposing:true})` 的 Enter 后值不变、无提交。**这条是弱证据**：合成事件不是真拼音选词，真 IME 仍需真人一次 |
| 历史抽屉 | 点开 `<details>` 后出现 4 行（c=8/7/6/5），由 `refresh_history` 向 `/api/v1/conversations` 取（200） |
| `/voice` 的 wasm 分支 | `kestrel_client.js` + `_bg.wasm` 都取到（304），页面上**没有** `.client-error`，控制台零条。按 `templates.rs` 的 `WASM_LOADER`，`initVoice()` 抛错必落 `console.error` + 页内提示——所以这是**它第一次执行到底**。（真麦克风没有测：按住要弹权限框） |
| GFM 子集上线生效 | 会话 8 的表格在浏览器里是真 `<table>`，不是裸竖线 |

### 22.2 不通过：`dataset()` 的 kebab 拼法（六处，五处承重）

`document.body.dataset` 的实际键是 `["conversationId","startSeq","canDeleteConversations"]`，
而 `dataset["conversation-id"]` 的类型是 `undefined`。客户端全部读取写成属性拼法，于是每个都是
静默 None。后果在浏览器里逐条取到：

| 现象 | 证据 |
|---|---|
| 已有会话里提交 → 新建会话 | 当前页 `/?c=9`，按 Enter 得到 `POST /api/v1/conversations` **201** 与 `POST /api/v1/conversations/10/turns` **202**，URL 变 `/?c=10` |
| 已有会话首屏根本不订流 | 重载 `/?c=9` 后网络面板只有 wasm 两个文件，**零个** `/api/v1` 请求、零个 `stream`——正在跑的轮次与后台视频完成事件永远到不了这一屏 |
| 删除控件全线失效 | 代码级：`can-delete-conversations` / `delete-conversation` / `delete-choice` 三处恒 None。**未页面复现**：这个账号是超管，服务端本来就不渲染删除 |

根因、六处清单与护栏测试见 implementation-notes.md §19；「新会话」那条独立的语义缺陷（服务端把无
`?c=` 的请求回落到最近会话，所以任何"重新加载拿一个空舞台"的做法都不成立）见 §19 的 11-06 更正。

### 22.3 修复与质量门（同日，三个提交）

`9a5d7d3` 六处键改 camelCase；`f40f132` 新增 `rust/client/src/dataset_keys.rs` 把这类拼法钉成静态
护栏（宿主 `cargo test` 可跑，不进任何包）；`e33a017` 提出 `reset_to_new_conversation()`，「新会话」
与"删掉当前这一会话"共用同一次客户端清场（对齐 `nest/client/scripts/main.ts` 的
`resetToNewConversation`）。质量门：**`cargo test`（client）39 passed**（护栏两条 + 原 37）、
**`cargo check --target wasm32-unknown-unknown` 零 warning**。

### 22.4 边界

- **修复尚未上线**：没跑 `make build-wasm`，没部署，线上还是坏的那份。22.1 那批通过项是对**坏包**做的，
  上线后需要重跑——尤其"已有会话首屏订流"与"多轮续接"这两条此前根本不成立。
- **仍未测**：出图（`image` 模式，会真扣额度）、视频异步任务、真麦克风通话、`/admin` 页面内容、
  下载对话框的 canvas 重编码。断线续传在修复前是结构性不成立，不是"没测"。
- **测试留下的数据**：账号 `tww316` 下新增会话 **c=9「回归测试第一行」**、**c=10「回归测试第二行：
  这是在 c=9 里提交」**，都是 `对话` 模式（零工具，无出图）。模式已在测完后还原为 `智能`。
- §21 那句"这一步只能由人做"**要更正**：chrome-devtools MCP 在本机可用，Agent 能在真人登录态的
  浏览器里跑完这条清单；只有需要权限框（麦克风）与需要人眼判断观感的步骤仍然只能真人。

## 23. 修复版上线，同一浏览器重跑走查（2026-11-06 13:58 起）

### 23.1 上线与拨档

| 步骤 | 结果 |
|---|---|
| 部署前备份 | `data/backup-20261106-pre-datasetfix.db`（172,032 B） |
| `make deploy` | 服务端二进制**没变**（`COPY .build/linux-x86_64/kestrel-studio → CACHED`），只有客户端换了；归档 4.8 MB，宿主机建镜像后 `deploy.sh` 的健康探测通过 |
| 产物一致性 | 线上 `kestrel_client.js` md5 `cb999e9285ab…`、`kestrel_client_bg.wasm` md5 `a073a9057662…`，与本机 `public/wasm/` 逐字节相同；wasm 本体 **375,413 B**（比 §20 那版 +1,040 B，就是这三笔修复的价钱） |
| 拨档 | `CLIENT_BUNDLE=wasm docker compose up -d` → 启动日志 `client="wasm"`。**这次没带 `RUST_LOG=debug`**：`info` 档下 5 分钟内 DEBUG 行计数 0，日志文件从 16 MB 的那份换成新容器的 4.0 KB。§21 与 deployment.md §2.1 记的"取证档位忘了收回"到此关闭 |

### 23.2 三条修复的复验（全部在真实浏览器里取到）

| 修复 | 判据与证据 |
|---|---|
| dataset 键（`conversationId` / `startSeq` / `messageId`） | 打开 `/`（回落到 c=10）后客户端**主动订流**：`GET /api/v1/conversations/10/stream?from=55`——`from` 是壳里 stamp 的游标，说明三个键都读到了 |
| 多轮续接 | 在 c=10 里按 Enter：`POST /api/v1/conversations/10/turns` **202**，**没有** `POST /api/v1/conversations`；URL 不变、`conversationId` 仍是 10、`.turn` 从 2 变 4、右侧索引从 1 行变 2 行 |
| 新会话清场 | 点「新会话」后：URL 仍是 `/`、`data-conversation-id` **不存在**、`#stream` 子节点 0、索引行 0 且 `#turn-index.hidden === true`、焦点落在 `#prompt`、历史列表刷新成 6 行、无 `.client-error` |
| replay 去重（`messageId`，此前被上面两条掩盖） | 见 23.4：轮次进行中重载页面后，该轮的用户气泡在 DOM 里**只出现 1 次** |

### 23.3 P1 剩下的几条判据，这次过了四条

| 判据 | 证据 |
|---|---|
| 出图（同步） | `图片` 模式提交 → c=11 新建（新会话后的首次提交，语义与 Nest 一致）→ 工具帧 `generate_image — Generated 1 image(s) at 1024*1024 with wan2.7-image … Asset ids: 18` → 页面出现 `/api/v1/assets/18/download` 的 `<img>` 与「下载」链接，库里 `assets` 行随之增加 |
| 下载弹窗的 canvas 重编码 | 打开时默认档 `100 × 100` → 预计 4.8 KB；把尺寸单选点到 `1024` → 预计 **186.6 KB**。我另外在页面里独立建 canvas 复算同一张图 q0.9 = 191 KB，同量级。**第一次在浏览器里验过**（§20 之前只有编译与宿主测试） |
| 视频（异步任务） | `视频` 模式在**已有** c=11 里提交（再次证明续接）→ 工具帧 `generate_video — Video task 3 submitted … 1280*720, 5s` → 本轮立即结束（composer 复位）→ 页面上出现"video 渲染中"的挂起条 → 几分钟后 `<video src="/api/v1/assets/19/download">` 落地、挂起条消失、`.turn` 由 4 变 5，**全程没有重载**。库侧对齐：`generation_tasks` 第 3 行 `kind=video status=succeeded asset_id=19` |
| `/admin` 页面内容 | `/admin` 列出 2 个账号（角色、会话数、资产数、创建时间、管理链接），`/admin/conversations?user=1` 的过滤与"已删除"标注正确，只读回放页 `/admin/conversations/3` 渲染 18 轮 / 11 个资产并带「只读视图，不能在此发送消息」；这些页面**不加载任何客户端脚本**（`script[src*=wasm]` 为空），控制台无 error/warning |

### 23.4 断线重连：过的是游标回放那条，不是 `Last-Event-ID`

做法与诚实的边界：

1. 先试了 CDP 的网络离线模拟——在一轮出图进行中 `Offline` 26 秒再恢复。结果图（asset 20）照样落到页面上、没重载。
   **但这不能算断线续传的证据**：网络面板自始至终只有**一条** `stream` 请求，说明那条 EventSource 根本没被拆掉，
   CDP 的 Offline 只是让新请求失败，没有撕开已经建立的流。
2. 于是改走真会断开的那条路：`对话` 模式提交一轮，**在轮次进行中重载页面**。重载后壳里 stamp 的是
   `data-start-seq="269"`，客户端立刻 `GET /api/v1/conversations/11/stream?from=269`，把这一轮的剩余事件接着画完，
   `.turn` 从 7 变 9，该轮用户气泡在 DOM 里**只出现 1 次**（`seed_rendered` 用 `messageId` 建的 replay 去重集合
   第一次在浏览器里生效——修复前它是空的，按理会看到两条）。

结论：**游标回放 = 通过**；**socket 被拆后由浏览器带 `Last-Event-ID` 自动重连 = 仍未测**。要真验后者，得让服务端
连接断掉（重启容器即可，但要人批准一次重启），或者拿真网络故障来做。不写成已验。

### 23.5 控制台与一处我自己的误判

- 全程（`/`、`/?c=`、`/admin` 三页、两次重载、两轮生成）**零 error、零 warning**。唯一一条是既有的 Chrome
  *issue*：`Lazy-loaded images should have explicit dimensions`，在产品页 count=2、在 `/admin/conversations/3`
  count=11——来源就是 §3 记的那条，2026-11-06 早上刚把它的线上出处改写成 `templates.rs::turn`；这次是它在
  新链路上第一次被量到，不是新缺陷。
- 我一度把"预计大小 4.8 KB"当成第四个缺陷。查了两个服务端的单选默认（`nest/…/chat.hbs:119` 与
  `rust/…/templates.rs:260` 都写死 `value="100" checked`），再点大档复算，才确认数字是对的。
  **记在此处是为了留个教训**：看到"不像话"的数字，先确认它对应哪一档，再决定是不是缺陷。

### 23.6 还缺什么

P1 现在只剩两条：**语音的真拨号**（要 `getUserMedia` 权限框 + 人耳听回放；`initVoice` 已在 §22 验过执行到底），
以及 **`Last-Event-ID` 的自动重连**。P4 的闸门：这两条之前不删 `nest/`。

本次留下的数据：会话 c=9 / c=10 / c=11（11 里有图 18、视频 19、图 20 与两轮对话），
`messages` 61、`conversation_events` 2,124、`assets` 20、`generation_tasks` 3。模式与档位都还原到位
（`智能`、`client="wasm"`、`info`）。

## 24. `Last-Event-ID` 自动重连（半证）与一次意外的新缺陷（2026-11-06 16:44–16:50）

### 24.1 先记运维那笔

`docker-compose.yml` 的 `logging` 段加了 `max-size: 10m` / `max-file: 5`，把文件传到宿主机后
`CLIENT_BUNDLE=wasm docker compose up -d` 重建生效。证据：`HostConfig.LogConfig` =
`{"Type":"json-file","Config":{"max-file":"5","max-size":"10m"}}`，启动日志仍是 `client="wasm"`，
`/login` 200，新容器日志当时 1,122 B。§23.1 关掉的只是档位，这条把根因也关掉了。

### 24.2 浏览器带 `Last-Event-ID` 这一半：证到了

| 时刻 | 事件 |
|---|---|
| 16:44:11 | `docker compose restart`（日志里 `signal received, closing the listener`）。第一条 stream 被真撕开，Chrome 在控制台记了一条 `Failed to load resource: net::ERR_HTTP2_PROTOCOL_ERROR`——断连的痕迹，不是客户端缺陷 |
| 16:44:22 | 容器回来，`client="wasm"`，`expired sessions cleared=0`（没人被登出） |
| 16:44:24 | 重连请求（reqid 6）：URL 仍是客户端建流时的 `?from=0`，但**请求头 `last-event-id: 1181`**，服务端回 200 `text/event-stream` |

服务端 `rust/server/src/http/chat.rs:213-216` 读这个头并**优先于 `?from=`**，所以「浏览器自动重连并带游标」
与「服务端认这个游标」两半都有实物证据。这也是 §23.4 里 CDP Offline 撕不开流所缺的那一半。

### 24.3 断开期间的缺口回放：没证到，而且原因本身就值得记

要造"断开期间仍在发布事件"，就得让某一轮在那 10 秒里继续跑。第二次尝试正是这样：16:46:2x 在**已有**
c=12 里提交一轮（`POST /api/v1/conversations/12/turns` 202——顺带第三次复验续轮），6 秒后重启，
重启瞬间 `max(seq)=2381`，最终 `max(seq)=2921`。

结果不是补播，是**那一轮跟着进程一起死了**：`conversation_events` 的末尾停在 `reasoning`，
**没有 `turn_done` 也没有 `turn_error`**。于是

- 页面一直卡在"正在生成…"（`sendDisabled=true`、`.content` 长度 0），直到重载才恢复；
- 重载后放出来的是库里那一行 **`本轮处理失败：服务在轮次完成前中断。`**（messages id=65），
  由启动收尾 `abandonStalePlaceholders` 写入；
- 同一次重载再次给出 `data-start-seq=2921` 与 `GET /12/stream?from=2921` 一致，游标读取依旧正确。

**结论：缺口回放仍记「未测」。** 要真验它，得让断开窗口里确实有事件发布——异步视频任务的完成时刻
不受控，或者得开第二个页面向同一会话发一轮。两条都比"留成未测"更贵，且更像是在凑证据。

### 24.4 顺带挖到一条新缺陷（两个客户端共有，不是 wasm 回归）

**现象**：服务端在一轮进行中被重启，那一轮不会有终态事件；正看着这一轮的页面**永久停在"正在生成…"**，
只有重载才恢复。

**两端同形**：`nest/client/scripts/main.ts` 的 `streamTurnRunning` 只在 `turn_done` / `turn_error`
与 `resetToNewConversation` 里复位，没有看门狗；`rust/client/src/chat_page.rs` 的 `turn_running` 一样
（整个文件里唯一的 `set_timeout` 是下载弹窗的 160 ms 防抖）。

**服务端其实没漏收尾**：启动时会把空占位行写成失败说明，但**这条收尾不产生事件**，所以活着的连接
永远收不到。修法两个方向：启动收尾顺手往 `conversation_events` 补一帧 `turn_error`（一次改动、
两个服务端同步、且天然被重放路径带上），或客户端加"订阅后 N 分钟无终态即重载"的看门狗。倾向前者。
**本节写下当天就已修**：两个服务端都补了这帧终态事件，见 §25.2；**尚未部署**。

**边界**：只验过"轮次进行中重启服务"这一种故障；进程 OOM、网络半开、反代超时没碰。

## 25. 语音第一次在新链路上拨号，以及启动收尾补的那帧终态事件（2026-11-06 17:07 / 18:0x）

### 25.1 `/api/voice` 的 upgrade 通了，音频没有

操作者在 `/voice`（灰度 `wasm` 档）按住两次，服务端 INFO/WARN 给出一条完整轨迹：

```
voice session opened conversation=13 user=2
voice turn committed frames=0
WARN voice turn failed Error committing input audio buffer: buffer too small, or have no audio. failures=1
voice turn committed frames=0
WARN voice turn failed … failures=2
voice call ended conversation="13"
```

**这条记录关掉的是 §13.5 与 §19 一直挂着的那件事**：WASM 语音客户端第一次取到 ticket、完成 WS
upgrade、服务端建立起上游会话并正常收尾——`initVoice` 与 `/api/voice` 不再是"零请求"。

**但 `frames=0` 说明音频一字节都没到服务端**。最可信的解释是首按正好撞上权限框：`getUserMedia`
在提示期间不产出，而 `capture.js` 是 1024 样本/帧、24 kHz 下约 **43 ms 一帧**，正常按住 1.2 秒该有
约 27 帧；两次 commit（按下后 1.2 秒与 4 秒）都可能落在麦克风真正打开之前。现在
`navigator.permissions.query({name:'microphone'})` 已是 `granted`，决定已持久化，所以**再按一次**
理应看到 `frames` 大于 0。

我没有代操作者按：合成一次 `pointerdown` 会在权限已授予的前提下悄悄打开麦克风，把房间里的环境声
送进外部 ASR——这件事只应由人自己按。所以 §3 第一条语音遗留项**仍然是"真实麦克风采集未验证"**。

**顺带在核心里看到一条缺口（本次未修）**：`voice_core::release()` 只防 `blocked`（上游刚拒绝过）
与 `busy`，并不知道"这一按到底有没有产出过帧"，于是静默按住也会发出 commit，由上游回
`buffer too small, or have no audio`。`abort()` 覆盖了"采集失败"（有测试
`an_uncapturable_press_is_abandoned_without_committing`），没覆盖"采集还没开始"。
补法很小：核心记一个"本次按住是否产出过帧"的标志，没有就跳过 commit 并复位状态——纯客户端核心
逻辑，宿主测试可钉。**等确认再做**。

### 25.2 启动收尾现在会补一帧 `turn_error`

§24.4 那条缺陷（轮次进行中重启服务 → 页面永久停在"正在生成…"）的修法选了服务端那一侧：

| 改动 | 内容 |
|---|---|
| `rust/server/src/conversation/repo.rs` | `abandon_stale_placeholders` 从 `u64` 改为返回 `Vec<(conversation_id, message_id)>`（`UPDATE … RETURNING`），收尾不再只改 `messages` 还知道该为哪些轮次发终态 |
| `rust/server/src/main.rs` | 启动时对每一对 `events.publish(TurnError{ message_id, "服务在轮次完成前中断。" })`；发布失败只 WARN，不阻塞启动（日志坏了不该让服务起不来） |
| `nest/server/src/conversation/conversation.service.ts` + `agent/turn-runner.service.ts` | 同一条契约的 Nest 侧镜像：先 `select` 出空占位行再 `update`，bootstrap 里逐条 `publish({type:'turn_error',…})` |
| 测试 | rust 侧在原 placeholder 测试里断言"这些对必须落成一帧 `turn_error`"（查 `conversation_events` 最后一行的 `type` 与 `message_id`）；nest 侧把原来断言条数的两处改成断言返回的 `{conversationId, messageId}` |

为什么这能解锁页面：两个客户端都在 `turn_error` 上把 `turn_running` 置回 false 并同步 composer
（`rust/client/src/chat_page.rs` 的 `Paint::TurnError`；Nest 侧同理），而重连后的客户端读的就是
这份日志。

**质量门**（沙箱外，见 implementation-notes.md §20 那条假红）：`nest/server` 28 files / **461 passed**、
`nest/client` **49 passed**、`cargo test`（server）**169 passed / 0 failed / 2 ignored**、
（client）**39 passed**；`make check` 两次 `tsc --noEmit` 与两次 `cargo check --tests` 全清。

**边界**：**尚未部署**，所以这条修复在线上还没有效果。验法现成——上线后重玩一次 §24.3 那个动作
（提交一轮、中途 `docker compose restart`），预期这次页面自己从"正在生成…"里出来，不需要重载。

## 26. 服务端修复上线并实测：终态事件靠游标重放解锁了页面（2026-11-06 17:24–17:27）

### 26.1 部署

| 步骤 | 结果 |
|---|---|
| 备份 | `data/backup-20261106-pre-bootrecovery.db`（679,936 B；白天这一天的走查把库从 172 KB 推到 680 KB，几乎全是 `conversation_events`） |
| 部署前基线 | `sessions` 3、`conversations` 13、`messages` 65 |
| `make deploy` | 这次 `COPY .build/linux-x86_64/kestrel-studio` 是 **DONE 而不是 CACHED**（服务端确实变了），镜像 `90c93fcd…`，健康探测通过 |
| 重拨灰度 | `CLIENT_BUNDLE=wasm docker compose up -d` → `client="wasm"`；`/login` 200、`/assets/main.css` 200、wasm 两个文件 200、`/admin` 302；`HostConfig.LogConfig` 仍是 `{"max-file":"5","max-size":"10m"}`；`expired sessions cleared=0`，`sessions` 仍 3——**没人被登出** |
| 客户端包 | md5 与 §23 那次相同（`cb999e92…` / `a073a905…`），本次只换服务端，符合预期 |

### 26.2 实测：点新会话 → 提一轮 → 中途重启

先在浏览器里点「新会话」（`conversationId=null`、`.turn` 0 条），再提交一轮 `对话`：新建 c=14、
`POST /14/turns` 202、`stream?from=0` 200。重启前 `max(seq)=908`。

```
17:26:17  before restart seq: 908        ← docker compose restart 发出 SIGTERM
17:26:28  Container Started
          INFO finalized assistant rows left by a previous process abandoned=1
          INFO kestrel-studio listening on http://localhost:8848 client="wasm"
          journal tail: 1424|text  1425|text  1426|turn_error
```

页面侧（**没有重载**）：`send.disabled=false`、占位文案回到"想让红隼帮你做什么？"、最后一条助手气泡
内容是 `服务在轮次完成前中断。`、类名 `content turn-error`、`.turn` 只有 2 条（没有重复气泡）。
网络面板里断开后多出一条 `GET /14/stream?from=0`（reqid 46），即 EventSource 的自动重连。

落库：`messages` id=67 `content = 本轮处理失败：服务在轮次完成前中断。`；
`conversation_events` seq 1426 `message_id=67`，payload
`{"type":"turn_error","messageId":67,"message":"服务在轮次完成前中断。"}`。

### 26.3 这条顺带把"缺口回放"证掉了

推理只依赖两件事的先后：**启动收尾发布 1426 的时刻在"listening"之前**（同一段 boot 代码里，
`finalized …` 那行 INFO 就在 `listening` 前两毫秒），那时服务端还没开始 accept，任何客户端都不可能
正连着。所以这条连接后来能拿到 1426，只能是**重连时带 `Last-Event-ID`、服务端按游标从日志重放**的结果。

反过来讲：如果服务端只接 live tail，这一轮早就没有后续事件了，页面会永远停在"正在生成…"——
那正是 §24.4 修复前实测到的样子。于是 **§24.3 记的"缺口回放仍未测"现在改为通过**，
而且是通过一条终态事件这种最苛刻的形式：断开 11 秒、期间新增了 500 多条事件、末尾还挂着进程。

一个附带观察：`docker compose restart` 不是瞬间杀死——SIGTERM 之后那一轮在宽限期内继续把 seq 从
908 推到 1425 才被终止。所以"轮次进行中被重启"这种故障，日志里会留下**半截流 + 一帧启动补发的
`turn_error`**，排查时按这个形状认。

### 26.4 控制台

断开那一下留下 `Failed to load resource: net::ERR_HTTP2_PROTOCOL_ERROR`——**这是被撕开的 SSE 自己
的记账，不是客户端缺陷**（§23.5 那轮"零 error、零 warning"是在没有人工中断的情况下取得的）。
除此之外仍然零条，唯一的 *issue* 还是既有的 lazy-image 那条。

### 26.5 P1 现状

§13.5 那五条里，**出图、视频任务、`/admin` 内容、SSE 续传（含缺口回放）四条已在新链路上实测通过**；
只剩**真实麦克风采集**一条——它需要人按一下（`§25.1`：upgrade 已通，两次 `frames=0`）。
本次留下的数据：会话 **c=14**（一条被中断的轮次 + 那帧终态事件）。

## 27. Nest 侧访问日志的形状，与「双后端只能按 origin 分」的源码依据（2026-11-07，单元层）

本轮**没有任何部署动作**（本机不启动服务、不启动 Docker），所以这一节是源码与单元层的证据，不是
运行时证据。运行时那半留给 Nest 那套真正重新上线的那天（roadmap.md §4.5 末「仍未落地的两件」）。

### 27.1 分流不能按路径：两套写的是同一个 cookie 槽位

| 事实 | 出处 |
|---|---|
| Rust 侧 cookie 名写死 `connect.sid`，属性手写成 `Path=/; HttpOnly; SameSite=Lax; Max-Age=…` | `rust/server/src/auth/session.rs:12` 与 `:58` |
| Nest 侧 `session()` 既没传 `name` 也没传 `cookie.path`，用的就是 express-session 的默认值——同样是 `connect.sid` + `Path=/` | `nest/server/src/main.ts`（cookie 只设了 `httpOnly` / `sameSite` / `maxAge`） |
| 两边签名口径不同（同 HMAC-SHA256，被签的字符串一个含 `s:` 前缀一个不含），且 `sess` 行只有 Nest → Rust 单向可读 | §13.4 |

**结论**：同一个 origin 下按路径分流，等于让两套互相覆盖对方刚写下的那个 cookie 值，再叠上签名口径
不同，用户每跨一次路径就被登出一次。**子域是不同的 host**，不带 `Domain` 属性的 cookie 因此天然隔离
——这是双后端并存唯一干净的分流形态，已按此写进 deployment.md §1.1 与 §3。

### 27.2 实现与单元层输出

- 实现：`nest/server/src/common/access-log.ts`，挂在 `main.ts` 静态资源处理器**之前**（否则
  `/assets/*.css` 不会留行，而 Rust 侧是留的）。开关 `ACCESS_LOG`（`configuration.ts` 的
  `readAccessLog()`，认 `1` / `true`，其余一律关）。
- 质量门：`pnpm --dir nest/server typecheck`（`tsconfig.json` 与 `tsconfig.test.json` 两份）通过；
  `vitest run __tests__/common/access-log.test.ts __tests__/config/configuration.test.ts`
  = **2 files / 42 passed**。
- 默认 sink 真跑出来的一行，即形状证据本身（Nest 的 Logger 在测试里直接打到 stdout）：
  `[Nest] 37935 - 11/07/2026 4:19:54 PM VERBOSE [access] method=GET uri=/ status=200 latency=0ms`
- 断言钉住的边界：关档时一行不写但仍然 `next()`；`finish` 与 `close` 都触发时**只写一行**（客户端中途
  断开的那次 SSE 正是要找的那一行）；时钟回拨夹到 `latency=0ms`；`uri` 保留 `?from=9`。

### 27.3 沙箱里的 nest 全量套件：6 个文件假红

`pnpm --dir nest/server test` → **336 passed / 0 failed / 139 skipped（共 475）**，另有 **6 个测试文件
加载失败**，报错全部是同一条 `Error: listen EPERM: operation not permitted 0.0.0.0`。这 6 个文件确实是
会真占端口的控制器测试（`grep -l "app.listen(" nest/server/__tests__` 命中数正好 6：admin / auth ×2 /
view / chat ×2）。与 §20 那条 cargo 假红同一类环境问题，**不是这次改动弄坏了什么**；判据与对 §20 末句
的更正记在 implementation-notes.md §21。

### 27.4 还没验的，别当已验

- 那行日志在容器里的实际形状与量级（未部署，deployment.md §2.3 因此是实现的记录而非取证的记录）。
- Nest 侧 `/api/voice` 的 upgrade 确实不经过 express（`voice/voice.gateway.ts` 接的是 HTTP server 的
  `upgrade` 事件，源码结构如此，运行时未取证据）。
- 两套各自的 `PUBLIC_BASE_URL` 与跨套帧 URL 的签名可达性（deployment.md §1.1 第 2 条）。
- 双进程同时启动共用一个 SQLite 文件（两侧建表 / 补列 / seed 都在启动时做，这个组合从没跑过）。

## 28. Nest 镜像在宿主机建起来跑通了，顺带实测「启动收尾会抓走对方的轮次」（2026-11-07）

全程没动活库：镜像构建之后，一次性容器挂在 `sqlite3 .backup` 出来的**库副本**上，只监听
`127.0.0.1:8849`，测完删容器与副本。活库那侧只做过 `mode=ro` 查询。

### 28.1 镜像

- `scripts/deploy-nest.sh` 归档 **96 MB**（`dist` + `node_modules` + `public` + `Dockerfile.nest` +
  `docker-compose.yml`）→ 宿主机解包 → `docker compose --profile nest build nest`。
- **单阶段镜像 534 MB**（历史两阶段版 1.27 GB）。基础层 `dockerproxy.net/library/node:24-bookworm-slim`
  已在宿主机缓存（230 MB），`golang:1.24` 不再被引用。
- better-sqlite3 **不需要在宿主机重编**：包内 `prebuilds/` 带着八个平台的 `.node`
  （`linux-x64`、`linuxmusl-x64`、`darwin-arm64`…），require 时按平台挑。原来按旧管线保留的
  `node-gyp rebuild` 实测只产出一个空 `build/Release/obj.target`，应用照样开库，于是从
  `Dockerfile.nest` 删掉了（台账 implementation-notes.md §22）。
- 容器起来即服务：`GET /login` **200**，路由表全部映射成功，`Nest application successfully started`。

### 28.2 那条 Critical 是实测出来的，不是推的

1. 活库当前 `role='assistant' AND content=''` 的行是 **0 条**——此刻没有在飞轮次会被误伤，
   但代码路径是通的。
2. 副本初始读数：`users=2 | messages=67 | conversation_events=6471 | turn_error=1`。
3. 手工植入一条空占位行，模拟 Rust 正在 streaming 的那一轮。
4. 起 Nest 容器，8 秒后读数：**占位行 0 条**，带「本轮处理失败：服务在轮次完成前中断。」的行
   由 2 变 **3**，journal 里 `turn_error` 由 1 变 **2**，末尾三行是
   `14|1427|turn_error` / `14|1426|turn_error` / `14|1425|text`（1426 那帧是 §26 那次启动收尾留下的）。
   → **Nest 一启动就把另一套在飞的那一轮判死，并替它发出终态帧**，与 architecture.md §13.2
   第 1 条预测一致。页面侧的症状会是：composer 提前解锁显示"失败"，随后又收到同一 `messageId`
   的真实内容。
5. 顺带一条幂等性正证：`users` 仍是 2 行（`1|admin|user`、`2|tww316|super`）——Nest 的 seed 与
   超管创建在已有库上没有重复插行。

### 28.3 访问日志在宿主机上的运行时形状

```
[Nest] 1 - 11/07/2026, 8:52:53 AM VERBOSE [access] method=GET uri=/login status=200 latency=19ms
[Nest] 1 - 11/07/2026, 8:52:53 AM VERBOSE [access] method=GET uri=/assets/main.css status=200 latency=11ms
[Nest] 1 - 11/07/2026, 8:52:53 AM VERBOSE [access] method=GET uri=/admin status=302 latency=3ms
[Nest] 1 - 11/07/2026, 8:52:53 AM VERBOSE [access] method=GET uri=/ status=302 latency=1ms
```

4 个请求 4 行，`ACCESS_LOG=1` 是开关（关档时一行不写，由单测钉着）；静态资源留行这点与 Rust 侧
对齐。**日志带 ANSI 色码**，`docker logs | grep` 前要先去色——同 §13.8 那条坑，这次也没绕过去。

### 28.4 边界：这不是「双后端可以上线了」

- **两套同时跑没有测过。** §13.2 那四处里只有第 1 处（启动收尾）本次被实测；worker 不 claim、
  `seq` 无 UNIQUE、内存单轮锁这三处都要并行才会兑现，单容器实验碰不到。
- 植入行是手工 `INSERT` 的，不是 Rust 真实生成的轮次；谓词命中一致，但形状不完全等价。
- 第二后端**没接任何流量**：8849 只发布在宿主机 127.0.0.1（安全组仍只放行 80/443），openresty 里
  没有新 server 块，`nest` service 在 compose 里仍处于 profile 之后、未启动状态。
- `PUBLIC_BASE_URL` 跨套可达性仍未测（deployment.md §1.1 第 2 件配置项）。
- Rust 容器全程未受影响：`make deploy` 之后与本次实验之后各核对一次，`Up`、`/login` 200、
  `client="wasm"` 保持原样（§27.5 那笔）。
### 27.5 同日 16:28 的部署这一笔（它没有改变线上行为）

今天两个提交都没有动 Rust 服务端与客户端，所以 `make deploy` 走的是一条**镜像层全 CACHED**的路径：
`COPY .build/linux-x86_64/kestrel-studio` 与 `COPY public` 都命中缓存，宿主机上的 image 与 11-06 那份
同源——**线上字节没变**。真正发生的是**容器被重建了两次**：

1. `make deploy` 那次。它不转发 `CLIENT_BUNDLE`，所以重建把档位带回 compose 默认 `nest`——这是
   deployment.md §2.2 第 2 条那条性质的又一次兑现（每次部署都要重拨一次，不是可选动作）。
2. 按 §23.1 那条命令重拨：`CLIENT_BUNDLE=wasm docker compose up -d`。

核对到的（全程只读）：容器 `Up`、启动行 `kestrel-studio listening on http://localhost:8848
client="wasm"`、容器 env `RUST_LOG=info`（11-06 收回的 debug 档没有被这次部署带回来，`make deploy`
同样不转发它）、`HostConfig.LogConfig = {"max-file":"5","max-size":"10m"}`（§24.1 那条上限只在
**重建**时生效，这次正好是重建）、宿主机本地 `/login` 200、公网 HTTPS `/login` 200、
`/wasm/kestrel_client_bg.wasm` 200、`sessions` 表 3 行。

没取到的两条，别混进上面那张表：**登出与否没有前后基线**——「重建容器不登出任何人」是 §20 验过的
性质，这次按该性质认定，不是这次的实测；而 **`ACCESS_LOG` 在线上还没有对应物**——镜像里只有 Rust 二
进制，Nest 那套还没重新上线（roadmap.md §4.5），所以 §2.3 那条开关命令今天仍然只是实现的记录。

## 29. `?new=1`：空舞台第一次有了自己的 URL（2026-11-08）

写这一节时还没有任何部署动作，所以前半是源码与单元层的证据，§29.1 是缺陷在**修复前的线上包**上的复现；
同日 17:58 部署 + 重拨 `wasm` 之后，修复后的那一屏在同一个浏览器里复跑关闭（§29.2 末）。

**报来的现象**：点「新会话」，内容区清了；刷新浏览器，那条会话又完整出现在内容区。
**不是缓存**（`localStorage` 只有 `kestrel-studio:mode`；SSR 的 HTML 不带 `Cache-Control`），是
`GET /` 在 `?c=` 缺失时回落到「最近更新的那条」。§22.3 那次修复把地址栏改成裸 pathname，可回落不看
query——`/` 画的仍是最近一条，所以那次修的是「这一屏」，用户报的是「下一屏」。根因与判据记在
implementation-notes.md §23，首屏三条选择规则现在写在 architecture.md §4.1。

**改法**：`?new=1` 两端同形（`rust/server/src/view/controller.rs::chat`、
`nest/server/src/view/view.controller.ts::chat`），两个客户端把地址栏写成 `pathname?new=1`
（`reset_to_new_conversation`、`resetToNewConversation`）。值只认 `1`，其余写法等同于不带。

**质量门读数**：

| 门 | 结果 |
|---|---|
| `cargo test`（rust server） | **168 passed** + 2 条假红（`admin::removing_an_account…` 与 `http::assets_follow_ownership…`，都是 `PermissionDenied` 建 storage 目录，§20 同一条判据）；新增那条单独跑 `ok` |
| `vitest run __tests__/view/view.controller.test.ts` | **35 passed**（沙箱外跑的——这六个文件之一会 `app.listen(0)`，沙箱内是 §21 那条 EPERM 假红） |
| `pnpm --dir nest/client test` | **2 files / 50 passed**（比 §22.3 那轮多一条：新会话按钮写 `?new=1`；删当前会话那条的地址栏断言同步改成 `?new=1`） |
| 两份 `tsc --noEmit` | 通过（`nest/server` 的 tsconfig 与 tsconfig.test 各一份） |
| `cargo test`（rust client） | **39 passed**；`cargo check --target wasm32-unknown-unknown --tests` 无 warning |

**钉住的边界**（`rust/server/src/http/tests.rs::the_new_flag_paints_the_empty_shell_that_survives_a_reload`
与 nest 侧同名用例）：带标记时 `data-conversation-id=""`、`data-start-seq=""`、无 `<article>`、侧栏那一行
还在但不高亮；不带标记时同一路径画的仍是最近一条；`?new=on` 不认；零会话账号两种请求渲染结果相同。

### 29.1 缺陷在修复前的线上包上复现了一次（2026-11-08，chrome-devtools MCP，新标签页）

档位在 `wasm`，线上跑的是 11-03 13:21 那版包（§27.5：11-07 那次 `make deploy` 镜像层全 CACHED，字节
没变），所以这一屏测的是**将被替换掉的行为**，不是这次的改动。读数：

| 动作 | 地址栏 | `body[data-conversation-id]` | `#stream` 里的 `<article>` | 侧栏高亮 |
|---|---|---|---|---|
| 打开 `/` | `/` | `14`（`data-start-seq="1426"`） | 2 | 「请分十五个要点…」 |
| 点「新会话」 | `/`（`search=""`） | 无 | **0** | 无 |
| 刷新 | `/`（`search` 仍是 `""`） | **又变回 `14`** | **2** | 又亮 |

刷新后 URL 一个字都没变而内容区回到了会话 14：画它的不是缓存、也不是地址栏里的残留，是服务端
`GET /` 在 `?c=` 缺失时的回落。顺带两条——**控制台零条**（复现全程的导航，含那次刷新）；
`fetch('/?new=1')` 在线上这一版仍返回 `data-conversation-id="14"` + 2 个 `<article>` + 1 个高亮行，
即**这个标记还没上线**，本次改动目前只在仓库里。全程零 `POST`（空舞台上没有 id 可提），用完的标签页已关。

### 29.2 修复后的那一屏还没验（下次部署后照这张表反着走）

前提是按 §23.1 重拨过 `CLIENT_BUNDLE=wasm`——`make deploy` 本身会把档位带回 `nest`。

1. 打开一条已有会话 → 点「新会话」→ 刷新。预期地址栏 `/?new=1`、内容区空、侧栏无高亮、
   控制台零条、**不发任何 `POST`**（没有会话 id 可提）。
2. 在这个空舞台上发一句话 → 刷新。预期地址栏已被 `create_conversation` 改写成 `/?c=<新 id>`、
   那一轮由 SSR 回放 + 事件流接上，`?new=1` 不残留。

**同日 17:58 部署后两条都过了**（提交 `79a177e`，chrome-devtools MCP 新标签页，真人登录态）：

- 部署：`make deploy`（build-linux + build-wasm + build-web + deploy.sh）→ 新镜像
  `sha256:36066b588a2d…`、容器 Recreated/Started；它不带 `CLIENT_BUNDLE`，档位按 §2.2 回到 `nest`，
  随后 `CLIENT_BUNDLE=wasm docker compose up -d` 重拨，启动行 `kestrel-studio listening on
  http://localhost:8848 client="wasm"`。宿主机只读核对：`Up`、
  `logs={json-file map[max-file:5 max-size:10m]}`、`restart=unless-stopped`、本地 `/login` 200、
  以 `mode=ro` 打开库查得 `conversations` 15 行 / max id 15。
- 第 1 步：`/` 画的仍是最近一条（c=14，2 个 `<article>`）→ 点「新会话」→ 地址栏 `/?new=1`、
  `data-conversation-id` 消失、`<article>` 归零 → **刷新后地址栏仍是 `/?new=1`，
  `data-conversation-id=""`、`data-start-seq=""`、`<article>` 0 个、侧栏 10 行无一高亮**。
  §29.1 那张表里"刷新又变回 14"的那一行到此关掉。全程零 `POST`。
- 第 2 步：在这个空舞台上选「对话」模式发一句话 → 提交后地址栏被改写成 **`/?c=15`**（`?new=1`
  没残留）→ 那一轮流式跑到「新会话可用」→ 刷新 → SSR 回放 2 个 `<article>`、侧栏高亮在 c=15、
  `data-start-seq="4"`。**留下的数据**：会话 c=15（一条验证用的对话轮次，未删）。
- 控制台：整轮（三次导航 + 一次真轮次）**零条**。

## 30. 快捷键提示搬进输入框：显隐与几何（2026-11-08，chrome-devtools MCP）

按用户要求把页脚那行 `Enter 发送，Shift + Enter 换行` 移进输入框，聚焦显示、失焦隐藏。设计口径与
那条跨模板的耦合写在 architecture.md §8。

**量法**：本轮不构建、不起服务（硬规则），所以拿一份**照模板逐字写的 composer 骨架**
（`~/tmp/kestrel-hint/composer.html`，用完已删）在 `file://` 下 `<link>` **真实的**
`nest/client/styles/main.css`，在 Chrome 里按四个状态量 `getBoundingClientRect()` 与
`getComputedStyle()`，并把 TS 客户端那段 auto-grow（`min(scrollHeight, 200)`）照搬进去复现多行态。
读数（视口 1440×812）：

| 状态 | `opacity` | `visibility` | 提示盒高 | 提示顶 vs textarea 内容底 | `.composer-form` 高 |
|---|---|---|---|---|---|
| 未聚焦 | `0` | `hidden` | 11 px | 772 ≥ 769 | 82 px |
| 聚焦（空） | `1` | `visible` | 11 px | 772 ≥ 769 | 82 px |
| 聚焦 + 四行（长到 130 px） | `1` | `visible` | 11 px | 772 ≥ 769 | 随输入增高 |
| 焦点在发送键上 | `0` | `hidden` | 11 px | —— | 82 px |

三条结论：提示整个落在 textarea 的 14 px 下内边距带里（那条带不放正文，所以多行也不会被压住）；
`absolute` 出画入画都不占位，焦点切换时表单高度**没变过**（82 px 三次一致）；触发器用
`.composer-input:focus ~ .composer-hint` 而不是 `:focus-within`，最后一行就是这条的实测——焦点在
发送键上时提示不出现。**控制台零条**，截图目视通过（提示在框内右下、发送键左侧那一角）。

### 30.1 同日 17:58 部署后，真实页面那一屏也量过了

`/public/` 是构建产物（`.gitignore` 第 16 行），CSS 经 `make build-web` 落进镜像、模板经
`make deploy` 上线（部署读数见 §29.2 末）。在 `try.kestrel.justwork.link`（`wasm` 档）上：

- `fetch('/assets/main.css')` 里 `.composer-field` 与 `.composer-input:focus~.composer-hint`
  两条规则都在；DOM 上 `p.composer-hint` 的父元素就是 `composer-field`。
- 失焦 `opacity=0 / visibility=hidden` → 聚焦 `opacity=1 / visible` → 再失焦回到 `0/hidden`；
  三个状态里提示盒都落在 textarea 的下内边距带内（`insideBand` 与 `insideBox` 全为真）。
- 一条随之而来的性质，记下来免得日后当 bug 查：**那行字在无障碍树里也跟着焦点进出**——
  `visibility: hidden` 会把它从 a11y 树剪掉，失焦时的快照里没有它、聚焦时的快照里有六个节点
  （`Enter` / ` 发送，` / `Shift` / ` + ` / `Enter` / ` 换行`）。这与既有的 `.logout-form`
  用的是同一套显隐写法（architecture.md §8），要它常驻可读就得换成不剪 a11y 树的另一种显隐。
- 控制台零条。

## 31. 三处写加上所有权：质量门读数（2026-11-09，**单元层**，两套没并行跑过）

architecture.md §13.2 那三条 Critical/High 的代码改造（`messages.owner`、任务租约、`seq` 单写者 +
UNIQUE 游标索引）两侧同改。改法与理由在那节和 implementation-notes.md §24，这一节只有读数。

**这一节不是「双后端可以上线了」的证据**，正如 §28.4 那条边界：两侧从没同时对着同一个文件跑过，
本节的每一条都发生在**一个进程 + 一个内存库**里。并行是下一条，见本节末。

### 31.1 落地面

| 侧 | 改动 |
|---|---|
| Rust | `common/writer.rs`（新，`WRITER_ID="rust"`）、`db.rs`（3 个 `ADDED_COLUMNS` + `enforce_unique_event_seq` + 改掉那句与实现不符的 `BEGIN IMMEDIATE` 注释）、`conversation/repo.rs`（插入写 owner、收尾谓词三规则）、`conversation/events.rs`（`append`：单语句 INSERT…SELECT…RETURNING + 3 次重试 + `is_seq_conflict`）、`task/service.rs`（`claim` / `claim_as_writer`、投影两列）、`task/mod.rs`（`CLAIM_LEASE_MS`）、`task/worker.rs`（advance 之前认领） |
| Nest | `common/writer.ts`（新，`WRITER_ID='nest'`）、`database/database.module.ts`（同样 3 列 + `ensureUniqueEventSeq`）、`database/schema.ts`（3 列进 drizzle）、`conversation/conversation.service.ts`（三条插入写 owner、`abandonStalePlaceholders` 三规则）、`conversation/conversation-events.service.ts`（`append` 同形）、`task/task.service.ts`（`claim` / `claimAsWriter` + `CLAIM_LEASE_MS`）、`task/task.worker.ts`（advance 之前认领） |
| 客户端 | 一行没动。没有新 `TaskStatus`、没有新事件类型、模板与 `public/` 产物不变 |

### 31.2 质量门

| 门 | 读数 |
|---|---|
| `cargo test`（rust server，沙箱内） | **173 passed**、2 failed、2 ignored。那 2 条是 §20 记过的 storage 目录 `PermissionDenied` 假红 |
| 同 2 条，沙箱外复跑 | **2 passed** — 合起来 rust 侧 **175 条全过** |
| `cargo check --all-targets` | 干净，零 warning |
| `vitest run`（nest server 全量，沙箱内） | **342 passed**、141 skipped in 6 files（§21 那 6 个 `app.listen(0)` 假红文件） |
| 那 6 个文件，沙箱外复跑 | **141 passed** — 合起来 nest 侧 **483 条全过** |
| `tsc --noEmit`（`tsconfig.json` 与 `tsconfig.test.json` 各一份） | 都通过 |

新增/改写的用例 10 条：Rust 5 条（`the_journal_cursor_index_is_unique`、
`duplicate_cursor_sequences_are_renumbered_before_the_index_is_unique`、
`boot_recovery_only_ends_the_rows_this_stack_wrote`、`a_render_is_claimed_by_one_backend_at_a_time`、
`schema_is_idempotent_and_adds_later_columns` 补三列）；Nest 5 条（新文件
`__tests__/database/schema.test.ts` 3 条：补列且二次执行不重复补、UNIQUE 拦重复 seq、旧库重复挪到末尾后
才立 UNIQUE；`task.service.test.ts` claim 矩阵 2 条；`conversation.service.test.ts` owner 矩阵 1 条）。
两侧断言同形，因为两段 DDL 是手抄同步的。

### 31.3 单元层钉住的判据

- **收尾三规则**：本套的空占位无条件收尾；`owner='对方'` 的行内容仍是 `''`、且那条会话的 journal
  `latest_seq` 仍是 0（不认领就不该补终态帧）；`owner IS NULL` 且 `created_at` 在 5 分钟内不动，
  超过 6 分钟的收尾。
- **认领互斥**：无人持有→可 claim；同套再 claim→可（续租）；外来且 `claimed_at` 新鲜→不可；外来且
  过期 5 分钟零 1 秒→可；`succeeded` 的行→两侧都不可。`attempts` 在认领后仍是 0。
- **游标单写者**：`seq` 仍按会话从 1 连续（既有的 `seq_is_dense_per_conversation_and_starts_at_one`
  未改即过）；UNIQUE 索引存在；对 `(conversation_id, seq)` 重复的库，迁移后 `[(1,a),(2,c),(3,b),(4,d)]`
  ——原值的 keeper 是最小 rowid 那条，两个不可见的兄弟被补到末尾，之后 `COUNT(*) GROUP BY … HAVING >1`
  为 0。
- 迁移是幂等的：`apply_schema` / `applySchema` 连跑两次不报错、不重复补列、不重复改索引。

### 31.4 没验的，别当已验

1. **两个进程同时跑**——本节所有断言都是单进程 + 内存库。真实并行要看的三件事：两套的启动迁移在
   同一个文件上抢不抢、8s tick 交错时一个视频任务是否只 poll/镜像/落库一次、一帧 `seq` 在并发写之下
   是否真连续。**这是 §28 那次没做完的那一半**，仍在 `make nest-up` 前面。
2. **活库上的 UNIQUE 迁移**。线上那份库的 `conversation_events` 有两千多行，重复对数**未知**（旧时代
   单进程写，理论上不该有，但没人查过）。第一次启动要么打一条 `ERROR`（有重复，已修）要么静默；
   查法：`SELECT conversation_id, seq, COUNT(*) FROM conversation_events GROUP BY 1,2 HAVING COUNT(*)>1`。
   `docker-compose.yml` 挂的是同一个 `./data`，所以这一步在 `make deploy` 那一刻就发生，不需要第二后端。
3. **发布窗口的形状**：一侧新代码 + 另一侧旧代码同时在写。`SEQ_RETRY_LIMIT=3` 与 UNIQUE 是为这个
   窗口准备的，但窗口本身没实测过；旧代码那侧的 `MAX+INSERT` 仍可能落重复对，直到它也升级。
4. **provider 层的重复计费**是否真被止住——这只在并行实测里有意义，单元层只证了认领互斥。
5. wasm 客户端与 `CLIENT_BUNDLE` 档位不受影响，未重拨、未部署、未走浏览器。

> 上面第 2、5 两条写于本节（当天上午，尚未部署）。同一天 09:49 之后已被 §32 与 §33 取代：
> 活库的迁移走过（0 对重复，未重编号任何行）、`wasm` 档重拨并走过浏览器一屏、启动收尾的 owner 规则
> 在线上重演过。第 1 条（并行）与第 3、4 条仍然成立。原文按本文件的惯例不改。

## 32. 这一版上线：活库的迁移走完了，新写入带上 owner（2026-11-09 09:49 部署）

§31 那三处改造的代码上线。这一节是宿主机上的机器证据 + 同一浏览器里的一屏。

### 32.1 部署前的预检（停容器，取快照，本地只读）

按宿主机既存约定先做快照：`docker compose stop` → `-wal` 与 `-shm` 消失（干净关闭，WAL 已
checkpoint）→ `cp -a data/kestrel-studio.db data/backup-20261109-pre-ownedwrites.db`（两边 md5 相同
`65d3bf0670a09e100a7983e8ba954f35`，872 KB，这是 `data/` 里第 7 份备份）。取回本地用 python 的
sqlite **3.53.4** 只读打开查（宿主机自带的是 3.7.17，见 32.3 那句）。读数：

| 查的 | 结果 | 对本次迁移意味着 |
|---|---|---|
| `messages` 列 | 无 `owner`；`mode` 在末尾 | 走 `ALTER ADD COLUMN` 那条路径 |
| `generation_tasks` 列 | 无 `claimed_by` / `claimed_at` | 同上 |
| `pragma_index_list('conversation_events')` | `idx_events_conversation` 的 `unique=0` | 索引存在但普通 → 迁移会 drop/create |
| 重复 `(conversation_id, seq)` 对 | **0 对**（6,827 行事件，逐会话 `COUNT(*)=MAX(seq)` 全成立） | **不重编号任何数据**，修复循环空转 |
| 空 content 的 assistant 行 | **0 行** | 启动收尾无事可做，不该出现 `abandoned=` |
| 非终态任务 | **0 行**（4 个任务全 `succeeded`） | claim 路径不会动到既有行 |
| 行数 | users 2 / conversations 16 / messages 76 / assets 23 / events 6,827 / tasks 4 / sessions 3 | — |

也就是说这次部署只可能走温和路径：三条 `ALTER`、一次索引替换、零行数据改写。

### 32.2 部署与档位

`make deploy`（build-linux + build-wasm + deploy.sh）→ 新镜像
`sha256:0d53ed29f3bbc9b12d44efa62aa7f76647aad15aa7684492491050ee6dedcf4b`，容器 `Recreated`/`Started`，
`/login` 200。启动日志三行、**ERROR 与 WARN 各 0 条**，没有 `finalized … abandoned` 那行（与 32.1 的
0 空占位一致），也没有那条"修了 N 个重复游标"的 `ERROR`（0 对重复）。

`make deploy` 不带 `CLIENT_BUNDLE`，档位按 §2.2 回到 `nest`——启动行随之变成 `client="nest"`，
随后 `CLIENT_BUNDLE=wasm docker compose up -d` 重拨，启动行 `kestrel-studio listening on
http://localhost:8848 client="wasm"`。顺带核对：`running`、`json-file max-size=10m max-file=5`、
`restart=unless-stopped`。

### 32.3 活库的 schema 回查

宿主机自带的 `sqlite3` 是 **3.7.17**（2013 年的），它**不支持 `pragma_table_info('x')` 这种表值
函数**——写成 `SELECT … FROM pragma_table_info(...)` 会报 `no such column: (`,看着像语法错而不是版本问题。
得用经典 `PRAGMA table_info(messages);`。（另外 `pragma_index_list` 的列里没有 `sql`，索引定义要从
`sqlite_master` 取。）

- `messages` 第 8 列 `owner|TEXT`（可空，追加在表尾）
- `generation_tasks` 第 16、17 列 `claimed_by|TEXT`、`claimed_at|INTEGER`
- `PRAGMA index_list(conversation_events)` → `idx_events_conversation` 的 `unique=1`
- `sqlite_master.sql` → `CREATE UNIQUE INDEX idx_events_conversation ON conversation_events(conversation_id, seq)`

一个不留神会被当成 bug 的性质：**活库的列序与新建于内存的库不同**（`ALTER ADD COLUMN` 只能追加在尾部，
而 DDL 里 `owner` 写在 `created_at` 之前）。所有读写都按列名，不看位置，所以两侧仍然同形；
`SELECT *` 的位置式读法在这里会出错——现有的 SQL 没有这种写法。

### 32.4 同一浏览器里那一屏（chrome-devtools MCP，新标签页，真人登录态，`wasm` 档）

| 动作 | 读数 |
|---|---|
| 打开 `/` | 画的仍是会话 16（`data-conversation-id="16"`、`data-start-seq="352"`、7 个 `<article>`），含一张图与一段视频——**部署前**就在库里的内容 |
| 点「新会话」 | `/?new=1`、`data-conversation-id` 消失、`<article>` 0、侧栏无高亮；textarea 自动聚焦且快捷键提示那行按 §30 出现 |
| 空舞台选「对话」发一句（`用一个词回答：红隼是什么`） | 地址栏被改写成 `/?c=17`（`?new=1` 不残留），一轮流式跑到「猛禽。」，composer 自己解锁 |
| 刷新 | SSR 回放 2 个 `<article>`、`data-start-seq="21"`、侧栏高亮在 c=17 |
| 控制台 | 首屏那一条是 §3 记过的既有 Chrome *issue*（`loading="lazy"` 缺尺寸，**count=2**，对应会话 16 的两张图）；**回放那一屏 0 条**。全程零 error、零 warning |

### 32.5 写路径的库侧证据（这次真正要证的三件事）

| 查的 | 读数 |
|---|---|
| 新行的 owner | `messages` id=77（user，len 12）与 id=78（assistant，len 3）**都是 `owner='rust'`** |
| 全表 owner 分布 | `NULL` 76 行 / `rust` 2 行——正是迁移期的形状：旧行无主，新行有主 |
| 会话 17 的 journal | 21 行，`MIN(seq)=1`、`MAX(seq)=21`、`COUNT(DISTINCT seq)=21`；类型序列 `user_message → turn_started → reasoning → text → turn_done` |
| 全库重复 `(conversation_id, seq)` | 0 对（游标索引此后会直接拒绝第二次落同值） |
| `generation_tasks` | 4 行全 `succeeded`，`claimed_by` / `claimed_at` 仍是 `NULL`——claim 只作用于非终态行，旧行不动是对的 |

**这一条是 §13.2 第 3 处第一次在活库上被证明**：新的单语句游标分配（`INSERT … SELECT MAX+1 … RETURNING`）
跑出来的 journal 连续、不重、且终态帧落到了 `turn_done`。

### 32.6 边界：这一节没证的

1. **并行仍未测**。`nest` service 还挂在 profile 上、没起过，两套从没同时对着这个文件写过。
   §31 是单元层，这一节是单进程线上层，两者都不代替那次并行实测。
2. **启动收尾的 owner 规则没在活库上走过**。它要"提交一轮 → 中途重启容器"（§26 那次的重演），
   而重启不在这次"部署"的范围里，所以没做，等确认。已知的替代证据只有 §31 那条单元断言。
3. **Nest 侧的同一批改动没上线**：`Dockerfile.nest` 的镜像是 11-07 建的那版，不含这次代码。
4. 视频任务这次没重跑（会话 16 里那段视频是部署前产的），所以**claim 在真实渲染上只走过一次单元断言**，
   线上这一版还没有一条非终态任务可查。

## 33. 启动收尾的 owner 规则在活库上实测（2026-11-09 09:58–10:00，两次容器重启，用户批准）

§13.2 第 1 条那个 Critical 的修复，之前只有单元断言（§31）。这次在**线上那个库**上跑了两遍，
因为「自己的行无条件收尾 / 对方的行不碰 / 无主行等宽限期」这三条规则里，前两条只有在**真有一个在飞的
轮次被杀**时才看得出区别。

### 33.1 造一个能区分三种 owner 的局面

宿主机时间 `1794189507619`（09:58:27 前后），往测试会话 17 里插三行 assistant 空占位：

| id | `owner` | `created_at` | 该被收尾吗 |
|---|---|---|---|
| 79 | `nest` | 新鲜 | **不该**——这是对面那套正在 streaming 的行 |
| 80 | `NULL` | 新鲜 | **不该**——无主且没过宽限期，归属不可判定 |
| 81 | `NULL` | 过期 6 分钟 | **该**——过了 5 分钟宽限期的无主行 |

`make deploy-restart`（= `docker compose restart`）之后：

```
INFO kestrel_studio: finalized assistant rows left by a previous process abandoned=1
id|owner|len        79|nest|0    80||0    81||18  本轮处理失败：服务在轮次完成前中断。
seq|type|message_id 22|turn_error|81
```

**只有一行被抓**，正是 81。79 与 80 的 `content` 长度仍是 0，journal 也只多了那一帧
（会话 17 此前是 21 条事件，之后 `seq=22` 是 `turn_error` 指向 message 81）。

### 33.2 再跑一遍真实的那次：一轮在飞时被杀

浏览器（真人登录态、`wasm` 档、同一个标签页）在会话 17 里用「对话」模式提交一句长指令
（`分二十个要点逐条详细展开…`），流式跑起来 6 秒后重启容器。重启前一刻库里的空占位有三行：

```
in-flight: id=79 len=0     ← 上一轮的合成外来行
in-flight: id=80 len=0     ← 上一轮的合成无主新鲜行
in-flight: id=83 len=0     ← owner='rust'，这一轮真的被杀在这里
```

重启后：`finalized … abandoned=1`——**三行里只抓走属于自己的那一行**，合成那两行第二次没被动。
journal 尾部 `1284|text → 1285|text → 1286|text → 1287|turn_error|message_id=83`（那一轮被杀前
已经流到 1,286 帧）。会话 17 共 1,287 行事件、`MAX(seq)=1287`、全库重复 `(conversation_id, seq)` 0 对。

**页面在没有刷新的情况下自己解锁了**：`#stream` 最后一条 assistant 变成「服务在轮次完成前中断。」，
composer 可输入。这一帧是发布在客户端断线之后、且新进程 accept 之前的一帧，它能到浏览器只能
是重连带 `Last-Event-ID` 被按游标重放的结果（网络面两次 `GET /api/v1/conversations/17/stream?from=22`
都是 200，第二次是重连）。这条与 §26.3 同形，差别是这次收尾谓词已经带上 owner 过滤。

刷新那一屏：`data-start-seq="1287"`、5 个 `<article>`、composer 未锁、**控制台 0 条**。

### 33.3 控制台那一条 error 的归因

在「提交 → 中途重启」那一屏里，浏览器报了 1 条 `[error] Failed to load resource:
net::ERR_HTTP2_PROTOCOL_ERROR`。它是**被掐断的那条 SSE 响应本身**（`/api/v1/conversations/17/stream`），
由这次重启造成，不是应用行为：紧接着一次干净刷新，控制台 0 条。记在这里是为了下次别把它当成回归——
判据是「同一条路径不做人为 kill 时是否复现」，而不是「出现过 error」。

### 33.4 测试数据是怎么清的（以及为什么不能用 UI 清）

登录用的 `tww316` 是**超管**，而删除会话的入口对超管刻意不渲染
（`rust/server/src/view/templates.rs:140` 的 `page.can_delete_conversations`，端点同一规则），
历史面板里因此没有 `data-delete-conversation` 按钮，UI 这条路走不通。

所以按会话 id 精确删（先查归属：`id=17 / title=用一个词回答：红隼是什么 / username=tww316 / msgs=7 /
events=1287 / **assets=0**`——没有资产意味着没有镜像文件要跟着删），删除顺序与应用的
`delete_conversations` 一致（事件、任务、消息、会话，一个事务）：

```
conv|msgs|events|empty_placeholders|conv_total|events_total
0   |0   |0     |0                 |16        |6827
```

`events_total` 与 `conv_total` 回到 §32.1 预检那两个数（16 个会话 / 6,827 行事件），全库空占位 0 行。
**这是本节唯一一处直接写生产库的动作**（三行合成占位 + 一次按 id 的删除），范围只限会话 17，
而会话 17 整个是本次与 §32 的测试产物。

### 33.5 边界

1. 合成行是用宿主机 `sqlite3`（3.7.17）写的，不是用应用路径写的。它能被新代码正确读到，
   这是「Nest 那套写的行 Rust 会怎么对待」的代理证据——**代理的是列的语义，不是 Nest 的真实写入**：
   真并行的形状（两侧同时持有、`attempts` 交错、8s tick 撞在一起）仍未测，见 §31.4 第 1 条。
2. 这一节证的是**收尾谓词的归属判断**，不是「跨套解锁」：79/80 两行之所以永远空着，正是因为
   Rust 不再碰它们；真要看到对面那套把它们收尾，得让 Nest 真的在跑（P4 取消后那是排期问题不是代码问题）。
3. 视频任务的 claim 仍未在线上走过一次真实渲染（§32.6 第 4 条不变）。

## 34. 两套同时跑同一个 SQLite 文件：并行实测（2026-11-09 10:49–11:0x，宿主机冒烟环境）

§13.2 那个「两套现在不能同时接同一个活库」的结论，从 11-07 起一直挡着 `make nest-up`。
这一节把它做完——**在库副本上**，两套同时跑，四组断言。

### 34.1 环境

- `make deploy-nest` 重建 Nest 镜像（把今天的改动带进去）→ `kestrel-studio-nest:latest
  db3602893dcb`，534 MB；此前宿主机那个是 11-07 建的，不含本次代码。**只建镜像，没启动**。
- 副本：`sqlite3 data/kestrel-studio.db ".backup '/tmp/kestrel-parallel/data/kestrel-studio.db'"`
  （3.7.17 支持 `.backup`，可在生产容器**正在跑**的时候取到一致副本；983 KB，`journal_mode=wal`，
  游标索引已是 UNIQUE——生产库上午刚迁移过，见 §32.3）。`cp -a storage/.` 一并复制（68 MB）。
- 两个一次性容器共享这同一份副本与同一份 `.env`（因此 `SESSION_SECRET` 相同）：
  `par-rust` = `kestrel-studio:latest`，`-e PORT=8848 -e CLIENT_BUNDLE=wasm --tmpfs /tmp`，
  宿主 `127.0.0.1:8850`；`par-nest` = `kestrel-studio-nest:latest`，`-e PORT=8849 -e ACCESS_LOG=1`，
  宿主 `127.0.0.1:8851`。**都只绑在 127.0.0.1**，安全组仍只开 80/443，公网进不来。
  生产容器 `kestrel-studio` 全程 Up 未动。

两条认证上的实情（记在 implementation-notes.md §26）：`.env` 里的 `SUPER_ADMIN_PASSWORD`
与库里 `tww316` 的哈希**已经不匹配**（口令是 `/admin` 换过的，重启不重置——这是设计，但副作用是
宿主机侧脚本没法再用 `.env` 登录），所以副本里建了一个一次性普通账号 `smoke-par`（口令只活在这份副本
里，随副本销毁）；顺带一条互操作事实：**同一个 bcrypt `$2b$` 哈希两套都验得过**（`location: /`，
两边都不是 `/login?error=1`）。

### 34.2 断言一：第二次启动的迁移不抢

Rust 先起，1.9 秒后 Nest 起，两者都跑 `apply_schema` / `applySchema`：

| 查的 | 读数 |
|---|---|
| `PRAGMA table_info(messages)` | `owner` 恰好一列（第 8 位），没被补第二遍 |
| `PRAGMA table_info(generation_tasks)` | `claimed_by` / `claimed_at` 各一列（第 16、17 位） |
| `PRAGMA index_list(conversation_events)` | 只有 `idx_events_conversation` 一个，`unique=1` |
| 两侧日志里 `error\|panic\|schema\|UNIQUE` | Rust 0 条，Nest 0 条 |
| `/login` | 两套都 200 |

### 34.3 断言二：`seq` 在两个进程同时写之下仍连续

两套**同时**各提一轮「对话」（会话 20 走 Rust、会话 21 走 Nest，两个 POST 同一秒发出）：

```
conversation_id|rows_|lo|hi|dist
20             |402  |1 |402|402      ← owner 全是 rust
21             |616  |1 |616|616      ← owner 全是 nest
dups=0        busy/error/panic: rust 0, nest 0
```

副本跑完整套实验之后的全局读数：**`dup_pairs=0`、`non_dense_convs=0`**（9,894 行事件、23 个会话），
也就是每个会话都满足 `COUNT(*) = MAX(seq)`。两套第一次在同一张 `messages` 表里可区分：
`owner='nest'` 8 行、`owner='rust'` 5 行，其余是副本自带的无主旧行。

### 34.4 断言三：启动收尾不抓对方——这次是**真实**的在飞轮次

§33 那次的「外来行」是我手写的合成行（代理证据）。这次不用代理：

| 时刻 | 读数 |
|---|---|
| Nest 会话 22 的一轮正在流式（`id=89 assistant owner=nest len=0`） | 提交 7 秒后 `docker restart par-rust` |
| 重启 +4 秒 | `id=89` **仍是 `len=0`**；`content LIKE '%中断%'` 计数 **0** |
| 一轮跑完 | `id=89 len=1485 owner=nest`，journal 尾 `916|text → 917|turn_done|89` |

**§28.2 那条 Critical 至此在真实条件下关闭**：改动之前，这一枪会替对方把那轮判死并补一帧 `turn_error`。

### 34.5 断言四：claim 的两个方向

先说成本考虑：一次真实视频渲染要几分钟并真实扣费，所以这里**不新起渲染**。用两种方式拿到同样的结论：

1. **自然争用**：插一个复用既有 provider 任务号的 queued 行（task 5）。Nest 的 tick 先抢到并一个
   tick 内跑完（`succeeded`、`claimed_by='nest'`、`asset_id=24`、`result_message_id=90`），
   Rust 的 tick 此后 6 次扫到同一张表都没能动它（`by=nest` 不变）。**没形成多 tick 争用**——
   任务太快进终态，所以这一条只算旁证，不算证明。
2. **证明**：只让 Nest 一套在跑（`docker stop par-rust`），插一行 `status='queued',
   claimed_by='rust', claimed_at=now`（= Rust 正持有且租约新鲜，这正是一个真实持有者的行形状）：

   ```
   foreign-lease tick1..3: queued attempts=0 by=rust      ← Nest 连续 3 个 tick 跳过它
   把 claimed_at 拨到 6 分钟前 →
   expired tick1: failed by=nest err=供应商返回状态 UNKNOWN  ← 下一 tick 立刻接管并推进
   ```

   新鲜外来租约不动、过期租约接管，两个方向都在**真实 worker** 上验过。
   顺带一条：Rust 自己那条探针任务（task 6）的失败文本是 `供应商返回状态 UNKNOWN`，Nest 接管后写出的
   文本**一字不差**——两侧这段错误形状是同形的。

副本里 `claimed_by` 的最终分布：`NULL` 4 行（副本自带的历史终态任务）、`nest` 2、`rust` 1。

### 34.6 附测：§13.2 第 4 条真发生时会怎样

「每会话只一轮」的锁按决策**不改代码**、由入口按子域固定来挡。既然环境在手，就把它真的踩一次：
同一个会话 24，Rust 与 Nest 各提一轮，两个 POST 都回 **202**（两侧的进程内锁互相看不见，与文档一致）。
结果：

- journal 1,126 行、`seq` 1..1126、distinct 1,126、**两个 `turn_done`**、四条消息行 owner 各自正确
- 两侧日志里 `collided|busy|unique` **各 0 条**——重试路径一次都没被触发，说明单语句游标分配的原子性
  在真实并发下成立（不是靠重试兜过去的）

所以这条暴露的**只有钱**（同一会话两轮 = 两次真实请求），数据面在今天的改造之后是安全的。
这条从「不知道后果」变成「后果有边界且已知」，是把入口固定当护栏的正当理由，而不是偏好。

### 34.7 拆场与生产面

`docker rm -f par-rust par-nest` + `rm -rf /tmp/kestrel-parallel`（副本与副本专用账号一起销毁）。
拆完回查：`kestrel-studio` Up、`/login` 200、`live_convs=16 / live_events=6827 / live_assets=23 /
live_tasks=4`、`storage/` 23 个文件——**与 §33.4 结尾逐个相同**，本次实测没有一个字节落在生产库或
生产 storage 上。宿主机其余容器（四个 justwork API 与 1Panel openresty）全程未动。

### 34.8 这一节没证的

1. **跑的是库副本，不是活库。** 代码路径、共享文件的方式、WAL 与锁行为都相同，但活库上同时挂两套
   这件事仍然没发生过——它现在缺的不是判据而是入口（下一个待办就是 §4.5 那两个配置项）。
2. 一次真实渲染在两套同时扫同一行时的行为（本轮用「新鲜/过期租约」证明的是认领判定，不是渲染时长内
   的持续争用）。
3. 一侧新代码 + 一侧旧代码的发布窗口（§31.4 第 3 条）仍未测——两套这次都是今天这版。
4. 反代路由：两个子域各绑一套（deployment.md §1.1/§3）没做，所以真实用户流量仍以单套形态进线。

## 35. 空持按不再提交空缓冲：两端同改，单元层（2026-11-09，**未上线**）

§25.1 那条遗留（两次真拨号都 `frames=0`，上游回 `buffer too small, or have no audio`）里，
「语音的 WS upgrade 通了但音频一个字节都没到」这一半之外，还有一条相关缺口：
`voice_core::release()` 不知道**这一按有没有产出过帧**，静默按住也会 commit。本轮补上，两套同改。

形状：核心记一个**每次按住**归零的帧计数（`frames_this_hold` / `framesThisHold`，在 `audio_chunk`
里对每个被接受的块自增——问的是「这一按采到音了吗」，不是「送到 socket 了吗」，所以排队的块也计）。
计数为 0 时 `release` 与 25 秒上限两条路都**不提交**，改为闭合这一按：置 `blocked`（松开之后才到的
帧不能混进用户下一句）、丢 `queued`、还回按钮、并给用户一句
「这一按没有采到声音，没有提交；再按住说一次就行」——两套**同一句话**，因为两个 bundle 都在线。
`press` 清 `blocked`，所以下一按照常工作。

这不是 `abort` 那条路：那里页面知道自己被拒了（要麦克风没要到）；这里什么都没报错，只是一帧都没来。

| 门 | 读数 |
|---|---|
| `cargo test`（rust client，宿主层） | **42 passed**（原 39 + 新 3 条） |
| `cargo check --target wasm32-unknown-unknown --tests` | 干净 |
| `pnpm --dir nest/client test` | **52 passed**（原 50 + 新 2 条） |
| `tsc --noEmit`（nest/client） | 通过 |

**改两条既有用例的理由**：它们的剧本本身就是「按住、一帧音频都没有、松开/到上限」——
在旧形状下那正是被提交空缓冲的路径。`rust` 的 `a_long_hold_is_committed_at_the_cap…` 与
`a_press_is_refused_while_a_turn_is_running`、`nest` 的「ready 之前/之后松开按钮…」与
「本轮说完之后，下一次按住…」都补了一次 `emitMicChunk()`，让「有话要说」这个前提显式成立；
上限那两条的期望帧序因此从 `[start, commit]` 变成 `[start, audio, commit]`。

**未做的**：没构建 wasm、没部署、没在浏览器里真按过一次（真实麦克风采集那条仍开着，见 §3 与 §13.5）。
所以这一节是单元层的，不能替 P1 打勾，也不能当作「frames=0 从此消失」的线上证据。

## 36. 两套 markdown 渲染器的真实分歧面（2026-11-09，普查 + 双跑对照）

§4.5 把「`kestrel-md` 与 `marked` 两套口径」从过渡态改成了常驻态，并写下「两侧渲染不一致要长期按
缺陷对待」。此前没有读数——只有印象。这次量了，两件事分开。

### 36.1 生产内容上：40/40 结构一致

`make census-prod`（`mode=ro`）的构造分布：分母 40 条助手消息，出现的只有**行首列表 16、粗体 5、
GFM 表格 1**，最长 2,043 字符；子集外那六类（原始 HTML、图片、任务列表、`~~~` 围栏、脚注、
setext 标题）**全为 0**。

然后把生产库那 40 条正文导出来（`id|hex(content)`，只读查询）在两套上各渲染一遍，比较**结构签名**
而不是字节：`marked` 与 `kestrel-md` 在空白与生成的 `id`/`class` 上必然不同，那些不是用户看得见
的差异；`href`/`src`/`alt` 保留，因为那些看得见。结果：

```
input=prod.hex messages=40 structurally_identical=40 differ=0
```

**今天线上没有一条内容让两套渲染得不一样的东西。** 所以「统一两套口径」这件事此刻没有被观测到的
缺陷撑着——这是对它的降级，不是对它的豁免（见 36.2）。

### 36.2 构造逐条双跑：18 类里 9 类分歧

生产只覆盖三种构造，不足以说明剩下的会怎样。于是给每个构造造一条样本双跑：

| 一致（9） | 分歧（9） |
|---|---|
| 粗体、行首列表、GFM 表格、反引号围栏、行内代码、链接、删除线、有序列表、ATX 标题 | 图片、**原始 HTML**、任务列表、`~~~` 围栏、脚注、setext 标题、嵌套列表、引用块、分隔线 |

分歧的形状分两类：

- **`kestrel-md` 当文本、`marked` 当标记**（子集外一律转义 vs 照单渲染）：图片（kestrel 里
  `![红隼](url)` 塌成一个带 `!` 的链接，marked 出 `<img>`）、任务列表（`<input disabled type=checkbox>`
  vs 字面 `[ ]`）、`~~~` 围栏（marked 出 `<pre><code>`，kestrel 把 `~~~js` 当**删除线**渲染成
  `<del>~js console.log(1)`——比不渲染更糟）、setext（`<h1>` vs 字面「标题 ===」）、
  引用块（`<blockquote>` vs 字面 `> 引用一行`）、分隔线（`<hr>` vs 字面 `---`）、
  嵌套列表（marked 出嵌套 `<ul>`，kestrel 吃掉缩进拍平）。
- **一条有安全重量**：原始 HTML。`前 <div class="x">块</div> 后` 在 `marked`（默认选项，
  v18 已无 `sanitize`）里**原样进 DOM**，在 `kestrel-md` 里被转义成源码。
  也就是说**两套在线后端的 HTML 注入策略不一样**——助手文本由模型产出，而模型可以被提示注入。
  这条与「渲染好不好看」不同级，是 §4.5 那句「按缺陷对待」的第一个具体实例，需要的是一个决定
  （修法见 roadmap.md §3）。

### 36.3 顺带修掉的：哨兵本身有盲区

上面那批分歧构造里，**引用块、分隔线、嵌套列表、有序列表、ATX 标题在 `scripts/markdown-census.sql`
里一条都没数过**。原来的 SQL 只盯「子集外会不会出现」，所以它看不见「子集外已经出现了却没人报警」——
哨兵数的是它想到的东西，不是会出事的东西。已补这五类（只用 `LIKE` 与 `char()`，宿主机 sqlite 3.7
没有 CTE 也没有 `instr()`），补完在生产上重跑并与一份 python 精确复核逐条对撞：

| 构造 | 普查 SQL | python 正则 |
|---|---|---|
| 引用块 `>` | 0 | 0 |
| 分隔线 `---`（排除表格的 `|---|`） | 0 | 0 |
| 嵌套列表 | 0 | 0 |
| 有序列表 `1.`–`9.` | 1 | 1 |
| ATX 标题 `#` | 0 | 0 |

LIKE 的近似没有漏计也没有误计。**当前生产内容仍然一条都不踩 36.2 的那九类**——但下次模型开始写
引用块时，`make census-prod` 会先看见。

### 36.4 方法与边界

对照用的脚手架（rustc 直接把 `crates/md` 编成 rlib 的双跑驱动、marked 侧的 node 脚本、结构签名比较器）
建在 `~/tmp/md-drift/` 下，**跑完即删**：里面含生产消息正文，不该在开发机上过夜。因此 36.1/36.2 的
复现要重跑这套脚手架，仓库里没有留下它——留下的只有 `scripts/markdown-census.sql` 的扩展。
结构签名有意忽略 `id`/`class` 与空白，所以「一致」的含义是**同一份可见文档**，不是同一段 HTML；
两边输出都非空、token 数逐条列出，避免「都渲染成空」被误判成一致。

## 37. 语音守卫上线，以及「没有第二个子域」这个决定（2026-11-09 11:37 部署）

### 37.1 这次部署只动了客户端

没有 schema 改动（上午那次已经迁移过），内容只有 §35 那两条客户端守卫。

| 步骤 | 读数 |
|---|---|
| 部署前 | `docker compose stop` → `Exited (0)`，`-wal`/`-shm` 消失；快照 `data/backup-20261109-pre-voiceguard.db`，与主文件 md5 同为 `4482a35ad7c811fe368847ee23600d88`（第 9 份约定快照） |
| `make deploy` | 新镜像 `sha256:afc804112672300830a59b13e32c3ea6738f37f4812e7557282c819f692c43de`，容器 `Recreated`/`Started`，`/login` 200 |
| 重拨 | `make deploy` 照 §2.2 把档位带回 `nest`，随后 `CLIENT_BUNDLE=wasm docker compose up -d` → 启动行 `client="wasm"`；`running`、`json-file max-size=10m max-file=5`、`restart=unless-stopped` |
| 启动日志 | 三行 INFO，**零 ERROR / 零 WARN**，没有 `finalized … abandoned`（库里没有待收尾的空占位） |
| schema 第三次启动仍幂等 | `PRAGMA index_list(conversation_events)` 仍是唯一一个 `unique=1`；`PRAGMA table_info(messages)` 末尾仍是 `7|mode`、`8|owner`，**没有补出第二遍** |

**上线判据取的是包体本身**，不是页面观感：`grep -c '这一按没有采到声音'` 对线上取回的
`/wasm/kestrel_client_bg.wasm` 得 **1**，对 `/assets/voice.js` 也得 **1**——两套 bundle 都带着新的
那句提示，所以守卫确实在字节里，而不只是「部署成功了」。

### 37.2 浏览器那一屏（新标签页，真人登录态，`wasm` 档）

`/voice` 控制台 **0 条**；`/` 只有 §3 记过的那条既有 Chrome *issue*（`loading="lazy"` 缺尺寸，
**count=2**，对应会话 16 的两张图），**零 error、零 warning**。

**这一节没有验的、也不能验的**：没有按下语音按钮。守卫那条路径（空持按不提交）要真的验证需要一次
真实按住，而**在权限已授的浏览器里由 Agent 合成按住，会悄悄开麦把环境声送进外部 ASR**——这正是
§25.1 以来就一直由人做、不由 Agent 代做的那件事。所以 §35 的单元断言 + 本节的包体判据是这次能拿到的
全部证据；**`frames>0` 的真实采集仍是 P1 开着的那一条**（§13.5 五条之外只剩它）。

### 37.3 「不需要第二个子域，没有第二个子域」（用户决定，2026-11-09）

这条撤销的是 roadmap.md §4.5「同日定下的三件」里的第 1 件（对外形态 = 反代按子域分，两套各一个域名）。
随之作废的还有 deployment.md §1.1 那两件配置项（openresty 新增 server 块、每套自己的
`PUBLIC_BASE_URL`）。

**它同时把「第二后端能不能对外」这个问题回答成了不能**，理由值得留在文档里，因为下一次有人想做这件事
会先假设「换个端口就行」：cookie 的归属只看 host 与 path，**端口不参与**（RFC 6265 的比较算法里没有
端口这一项），所以 `host:8848` 与 `host:8849` 共用同一个 `connect.sid` 槽位；按路径分同理（§27.1 已经
按源码算过一次）。两套剩下的唯一隔离维度就是**另一个 host**——而现在没有，也不打算有。

所以 `make nest-up` 的语义降级为：**宿主机内部的第二个实现，不接真实用户流量**。这反而让 §13.2 第 4 条
（每会话单轮的锁只在进程内存）更安全：不是靠「一个人恒定落在一套上」这个路由护栏，而是靠第二套压根
没有流量入口。**护栏的性质从「路由」变成「不对外」**，这一点写在 architecture.md §13.2。

边界照旧要说清：端口不参与 cookie 归属这一条是规范层面的判断，**本部署没有在浏览器上实测过**
（宿主机的 8848/8849 只绑 127.0.0.1，安全组只放行 80/443，浏览器进不来）。
