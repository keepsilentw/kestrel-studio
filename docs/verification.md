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
"panic 机制 + 绑定层"这种不可压缩的 WASM 开销。据此把 markdown 换成**服务端已有的
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
**未做**：线上未部署（要下一次 `make deploy` 才生效，同样不会登出任何人），浏览器侧照旧没有
运行时观察（模板仍不引 wasm，SSR 那侧的表格渲染要在页面上看才算验过）。
