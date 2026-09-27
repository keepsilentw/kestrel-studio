# kestrel-studio 实测与验证记录

> 带日期的证据台账。**结论会过时**——端点行为、模型行为都可能变，引用前先确认日期与适用范围。
> 设计决策本身见 [architecture.md](./architecture.md)。

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
