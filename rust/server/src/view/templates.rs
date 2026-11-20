use super::html::escape;
use super::render::{RenderedMessage, TurnAnchor};
use crate::task::TaskKind;

/// The pages, as functions.
///
/// The Nest server used Handlebars templates; there is no template engine in the
/// local registry, so each page is assembled here instead. The output is meant to
/// be interchangeable with `src/view/views/*.hbs`, which is why the class names,
/// the `data-*` attributes and the element ids are copied over verbatim: the
/// client in `nest/client/scripts/main.ts` is unchanged and looks every one of them up.

#[derive(Clone)]
pub struct ConversationEntry {
    pub id: i64,
    pub title: String,
    pub is_active: bool,
}

pub struct PendingTask {
    pub id: i64,
    pub kind: TaskKind,
    pub status_label: String,
}

/// Everything the chat page needs, by value: the caller assembles it from query
/// results and hands it over, and nothing has to outlive the render.
pub struct ChatPage {
    pub username: String,
    pub is_super: bool,
    pub conversations: Vec<ConversationEntry>,
    /// The conversation the page was painted from. Rendered into
    /// `data-conversation-id`, which is what the client re-submits on a send.
    pub active_conversation_id: Option<i64>,
    /// The journal position the page was painted at. Empty when there is no
    /// conversation: the client then opens the stream without `?from=` and the
    /// server decides where to catch up.
    pub start_seq: Option<i64>,
    pub messages: Vec<RenderedMessage>,
    /// Present when older messages exist; `messages` is already one window.
    pub older_href: Option<String>,
    pub pending_tasks: Vec<PendingTask>,
    pub turn_anchors: Vec<TurnAnchor>,
    pub can_delete_conversations: bool,
    /// Which client the page loads. From `AppConfig::client_bundle`; a test builds the
    /// struct directly, so the switch is asserted without touching the environment.
    pub client: crate::config::ClientBundle,
}

pub fn login_page(error: bool) -> String {
    let mut out = String::from(
        "<!DOCTYPE html>\n\
         <html lang=\"zh-CN\">\n\
         <head>\n  \
         <meta charset=\"utf-8\">\n  \
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  \
         <title>登录 - Kestrel Studio</title>\n  \
         <link rel=\"icon\" type=\"image/svg+xml\" href=\"/logo.svg\">\n  \
         <link rel=\"stylesheet\" href=\"/assets/main.css\">\n\
         </head>\n\
         <body class=\"login-page\">\n  \
         <form class=\"login-card\" action=\"/login\" method=\"post\">\n    \
         <img class=\"login-logo\" src=\"/logo.svg\" alt=\"\" width=\"56\" height=\"56\">\n    \
         <h1 class=\"login-title\">Kestrel Studio</h1>\n",
    );
    if error {
        out.push_str("    <p class=\"login-error\">用户名或密码错误</p>\n");
    }
    out.push_str(
        "    <label class=\"login-field\">\n      <span>用户名</span>\n      <input name=\"username\" type=\"text\" autocomplete=\"username\" required autofocus>\n    </label>\n    <label class=\"login-field\">\n      <span>密码</span>\n      <input name=\"password\" type=\"password\" autocomplete=\"current-password\" required>\n    </label>\n    <button type=\"submit\" class=\"login-submit\">登录</button>\n  </form>\n</body>\n</html>\n",
    );
    out
}

pub fn chat_page(page: &ChatPage) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>Kestrel Studio</title>\n  <link rel=\"icon\" type=\"image/svg+xml\" href=\"/logo.svg\">\n  <link rel=\"stylesheet\" href=\"/assets/main.css\">\n</head>\n");
    out.push_str(&format!(
        "<body data-conversation-id=\"{}\" data-start-seq=\"{}\" data-can-delete-conversations=\"{}\">\n",
        page.active_conversation_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        page.start_seq.map(|seq| seq.to_string()).unwrap_or_default(),
        page.can_delete_conversations,
    ));

    out.push_str(&header(page));
    out.push_str("<main class=\"stream\" id=\"stream\">\n");
    if let Some(href) = &page.older_href {
        out.push_str(&format!(
            "    <div class=\"load-older\"><a class=\"load-older-link\" href=\"{}\">加载更早的消息</a></div>\n",
            escape(href)
        ));
    }
    for message in &page.messages {
        out.push_str(&turn(message));
    }
    for task in &page.pending_tasks {
        let label = match task.kind {
            TaskKind::Video => "视频",
            TaskKind::Image => "图片",
        };
        out.push_str(&format!(
            "    <div class=\"task-pending\" data-task-id=\"{}\">\n      <span class=\"task-spinner\" aria-hidden=\"true\"></span>\n      {label}任务 #{} {}…\n    </div>\n",
            task.id, task.id, escape(&task.status_label)
        ));
    }
    out.push_str("  </main>\n");

    if !page.turn_anchors.is_empty() {
        out.push_str("  <nav class=\"turn-index\" id=\"turn-index\" aria-label=\"会话节点\">\n    <p class=\"turn-index-title\">会话节点</p>\n    <ol class=\"turn-index-list\" id=\"turn-index-list\">\n");
        for anchor in &page.turn_anchors {
            out.push_str(&format!(
            "      <li>\n        <button type=\"button\" class=\"turn-index-link\" data-target=\"{}\" title=\"{title}\">{title}</button>\n      </li>\n",
                anchor.message_id,
                title = escape(&anchor.label)
            ));
        }
        out.push_str("    </ol>\n  </nav>\n");
    }

    out.push_str(COMPOSER_AND_DIALOGS);
    out.push_str(&client_script(page.client, "/assets/main.js", "initChat"));
    out.push_str("</body>\n</html>\n");
    out
}

fn header(page: &ChatPage) -> String {
    let admin = if page.is_super {
        "      <a class=\"menu-button\" href=\"/admin\">管理</a>\n"
    } else {
        ""
    };
    let mut items = String::new();
    if page.conversations.is_empty() {
        items.push_str("          <li class=\"conversation-empty\">暂无会话</li>\n");
    } else {
        for conversation in &page.conversations {
            let active = if conversation.is_active {
                " is-active"
            } else {
                ""
            };
            items.push_str(&format!(
                "            <li class=\"conversation-item\">\n              <a class=\"conversation-link{active}\" href=\"/?c={}\">{title}</a>\n",
                conversation.id,
                title = escape(&conversation.title)
            ));
            // The delete control is rebuilt by `refreshConversationList()` in
            // nest/client/scripts/main.ts — keep the two in step. It is only rendered for a
            // normal account; the same rule is enforced in the endpoint.
            if page.can_delete_conversations {
                items.push_str(&format!(
                    "              <button type=\"button\" class=\"conversation-delete\" data-delete-conversation=\"{id}\" aria-label=\"删除会话 {title}\" title=\"删除会话\">删除</button>\n",
                    id = conversation.id,
                    title = escape(&conversation.title)
                ));
            }
            items.push_str("            </li>\n");
        }
    }

    format!(
        "  <header class=\"topbar\">\n    <a class=\"brand\" href=\"/\">\n      <img class=\"brand-logo\" src=\"/logo.svg\" alt=\"\" width=\"32\" height=\"32\">\n      <span class=\"brand-name\">Kestrel Studio</span>\n    </a>\n\n    <nav class=\"menu\">\n      <button type=\"button\" class=\"menu-button\" id=\"new-conversation\">新会话</button>\n      <a class=\"menu-button\" href=\"/voice\">语音</a>\n      <a class=\"menu-button\" href=\"/settings/tokens\">令牌</a>\n{admin}      <details class=\"menu-dropdown\">\n        <summary class=\"menu-button\">历史会话</summary>\n        <div class=\"conversation-panel\">\n          <input type=\"search\" class=\"conversation-search\" id=\"conversation-search\" placeholder=\"搜索会话\" aria-label=\"搜索会话\">\n          <ul class=\"conversation-list\">\n{items}          </ul>\n        </div>\n      </details>\n    </nav>\n\n    <div class=\"account\">\n      <div class=\"account-menu\">\n        <span class=\"account-name\">{name}</span>\n        <form action=\"/logout\" method=\"post\" class=\"logout-form\">\n          <button type=\"submit\" class=\"logout-button\">退出登录</button>\n        </form>\n      </div>\n    </div>\n  </header>\n",
        admin = admin,
        items = items,
        name = escape(&page.username)
    )
}

/// One stored turn, the counterpart of `views/partials/turn.hbs`.
pub fn turn(message: &RenderedMessage) -> String {
    let view = &message.view;
    let mut out = format!(
        "    <article class=\"turn turn-{role}\" data-message-id=\"{id}\">\n",
        role = escape(&view.role),
        id = view.id
    );

    if let Some(label) = message.mode_label {
        out.push_str(&format!("      <span class=\"turn-mode\">{label}</span>\n"));
    }

    if let Some(reasoning) = view.reasoning.as_deref().filter(|value| !value.is_empty()) {
        out.push_str(&format!(
            "      <details class=\"reasoning\">\n        <summary>思考过程</summary>\n        <pre class=\"reasoning-body\">{}</pre>\n      </details>\n",
            escape(reasoning)
        ));
    }

    // Tool calls are persisted on the message; replay them here so a reloaded page
    // shows the same "called X → got Y" trail the live stream built.
    if let Some(calls) = view.tool_calls.as_deref().filter(|calls| !calls.is_empty()) {
        out.push_str("      <ul class=\"tool-calls\">\n");
        for call in calls {
            out.push_str(&format!(
                "        <li class=\"tool-call\">{name} — {summary}</li>\n",
                name = escape(&call.name),
                summary = escape(&call.summary)
            ));
        }
        out.push_str("      </ul>\n");
    }

    if !message.content_html.is_empty() {
        out.push_str(&format!(
            "      <div class=\"content\">{}</div>\n",
            message.content_html
        ));
    } else if !view.content.is_empty() {
        // The user's words are plain text, printed escaped — the same thing
        // textContent does on the live path. Keeping the bubble free of block
        // children is also what makes its height hug the text.
        out.push_str(&format!(
            "      <div class=\"content\">{}</div>\n",
            escape(&view.content)
        ));
    }

    for asset in &view.assets {
        let media = match asset.kind {
            TaskKind::Video => format!(
                "        <video src=\"{url}\" controls preload=\"metadata\" class=\"asset-video\"></video>",
                url = escape(&asset.url)
            ),
            TaskKind::Image => {
                // The two attributes are the whole fix for the reflow: a lazy image
                // with a known aspect ratio reserves its box before it downloads, so
                // the text under it stops jumping as history loads.
                // Both or neither — a lone `width` with no `height` lets the browser
                // keep the default ratio and distorts the box instead of fixing it.
                let box_attributes = match (asset.width, asset.height) {
                    (Some(width), Some(height)) if width > 0 && height > 0 => {
                        format!(" width=\"{width}\" height=\"{height}\"")
                    }
                    _ => String::new(),
                };
                format!(
                    "        <img src=\"{url}\"{box_attributes} alt=\"生成的图片\" loading=\"lazy\" class=\"asset-image\">",
                    url = escape(&asset.url),
                    box_attributes = box_attributes
                )
            }
        };
        out.push_str(&format!(
            "      <figure class=\"asset\">\n{media}\n        <figcaption class=\"asset-actions\">\n          <a href=\"{url}\" download class=\"asset-download\">下载</a>\n        </figcaption>\n      </figure>\n",
            url = escape(&asset.url)
        ));
    }

    out.push_str("    </article>\n");
    out
}

/// The composer, the mode picker and the dialogs. The ids in here are the client's
/// contract: `requireElement` fails the page if one is missing.
const COMPOSER_AND_DIALOGS: &str = r#"  <footer class="composer">
    <div class="composer-modes" id="composer-modes" role="radiogroup" aria-label="生成模式">
      <button type="button" class="mode-option is-active" data-mode="auto" role="radio" aria-checked="true">智能</button>
      <button type="button" class="mode-option" data-mode="chat" role="radio" aria-checked="false">对话</button>
      <button type="button" class="mode-option" data-mode="image" role="radio" aria-checked="false">图片</button>
      <button type="button" class="mode-option" data-mode="video" role="radio" aria-checked="false">视频</button>
    </div>

    <form class="composer-form" id="composer-form">
      <div class="composer-field">
        <textarea
          id="prompt"
          class="composer-input"
          rows="1"
          placeholder="描述你想要的图片，例如：一只站在岩石上的红隼，清晨逆光"
        ></textarea>
        <p class="composer-hint"><kbd>Enter</kbd> 发送，<kbd>Shift</kbd> + <kbd>Enter</kbd> 换行</p>
      </div>
      <button type="submit" class="composer-send" id="send" aria-label="发送">
        <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
          <path d="M3 20.5 21 12 3 3.5 3 10l12 2-12 2z" fill="currentColor"/>
        </svg>
      </button>
    </form>
  </footer>

  <dialog class="preview" id="preview">
    <img class="preview-image" id="preview-image" alt="">
  </dialog>

  <dialog class="download-dialog" id="download-dialog">
    <h2 class="download-title">下载图片</h2>

    <fieldset class="download-group">
      <legend>尺寸</legend>
      <div class="size-options">
        <label><input type="radio" name="download-size" value="50">50 × 50</label>
        <label><input type="radio" name="download-size" value="100" checked>100 × 100</label>
        <label><input type="radio" name="download-size" value="200">200 × 200</label>
        <label><input type="radio" name="download-size" value="1024">1024 × 1024</label>
      </div>
    </fieldset>

    <div class="download-group">
      <label class="download-row" for="download-format">
        <span>格式</span>
        <select id="download-format">
          <option value="image/jpeg" selected>JPEG</option>
          <option value="image/webp">WebP</option>
          <option value="image/png">PNG</option>
        </select>
      </label>
    </div>

    <div class="download-group">
      <label class="download-row" for="download-quality">
        <span>压缩质量</span>
        <output id="download-quality-value" for="download-quality">90%</output>
      </label>
      <input type="range" id="download-quality" min="10" max="100" step="5" value="90">
      <p class="download-hint" id="download-hint"></p>
    </div>

    <p class="download-estimate" id="download-estimate">预计大小：计算中…</p>

    <div class="download-actions">
      <button type="button" class="download-cancel" id="download-cancel">取消</button>
      <button type="button" class="download-confirm" id="download-confirm">下载</button>
    </div>
  </dialog>

  <dialog
    class="conversation-delete-dialog"
    id="conversation-delete-dialog"
    aria-labelledby="conversation-delete-title"
  >
    <h2 class="conversation-delete-title" id="conversation-delete-title">删除这条会话？</h2>
    <p class="conversation-delete-target" id="conversation-delete-target"></p>
    <p class="conversation-delete-hint">删除后它不再出现在你的历史会话里，也无法恢复。如需保留已生成的图片或视频，请先下载。</p>
    <div class="conversation-delete-actions">
      <button type="button" class="conversation-delete-no" data-delete-choice="cancel">取消</button>
      <button type="button" class="conversation-delete-yes" data-delete-choice="confirm">删除</button>
    </div>
  </dialog>
"#;

/// Which client a page starts — both `/` and `/voice` go through here, so there is one
/// place that decides it. The `nest` branch is what production has always run; the `wasm`
/// branch is the P3 client (roadmap.md §4.3, switch semantics in docs/deployment.md §2.2).
fn client_script(client: crate::config::ClientBundle, legacy: &str, entry: &str) -> String {
    match client {
        crate::config::ClientBundle::Nest => {
            format!("  <script type=\"module\" src=\"{legacy}\"></script>\n")
        }
        crate::config::ClientBundle::Wasm => format!(
            "  <script type=\"module\" src=\"/assets/wasm-loader.js\" data-entry=\"{entry}\" data-legacy=\"{legacy}\"></script>\n"
        ),
    }
}

/// `GET /voice` - the second entrance. The server renders the shell only: a call
/// has no history to replay, so everything below the header is appended by
/// `nest/client/scripts/voice.ts` (docs/architecture.md §11).
pub fn voice_page(username: &str, is_super: bool, client: crate::config::ClientBundle) -> String {
    let admin = if is_super {
        "      <a class=\"menu-button\" href=\"/admin\">管理</a>\n"
    } else {
        ""
    };
    let menu = format!(
        "      <a class=\"menu-button\" href=\"/\">返回对话</a>\n      <a class=\"menu-button\" href=\"/settings/tokens\">令牌</a>\n{admin}"
    );
    let main = format!(
        "  <main class=\"voice-stage\">\n    <p class=\"voice-status\" id=\"voice-status\">按住按钮开始说话</p>\n    <ol class=\"voice-log\" id=\"voice-log\"></ol>\n  </main>\n\n  <div class=\"voice-controls\">\n    <button type=\"button\" class=\"voice-button\" id=\"voice-button\">按住说话</button>\n  </div>\n\n{}",
        client_script(client, "/assets/voice.js", "initVoice")
    );
    page_shell("语音", "voice-page", &menu, username, &main)
}

/// The user-facing pages outside the chat shell: `/voice` and `/settings/tokens`.
fn page_shell(title: &str, body_class: &str, menu: &str, username: &str, main: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>{title} · Kestrel Studio</title>\n  <link rel=\"icon\" type=\"image/svg+xml\" href=\"/logo.svg\">\n  <link rel=\"stylesheet\" href=\"/assets/main.css\">\n</head>\n<body class=\"{body_class}\">\n  <header class=\"topbar\">\n    <a class=\"brand\" href=\"/\">\n      <img class=\"brand-logo\" src=\"/logo.svg\" alt=\"\" width=\"32\" height=\"32\">\n      <span class=\"brand-name\">Kestrel Studio</span>\n    </a>\n\n    <nav class=\"menu\">\n{menu}    </nav>\n\n    <div class=\"account\">\n      <div class=\"account-menu\">\n        <span class=\"account-name\">{name}</span>\n        <form action=\"/logout\" method=\"post\" class=\"logout-form\">\n          <button type=\"submit\" class=\"logout-button\">退出登录</button>\n        </form>\n      </div>\n    </div>\n  </header>\n\n{main}</body>\n</html>\n",
        title = escape(title),
        body_class = escape(body_class),
        menu = menu,
        name = escape(username),
        main = main,
    )
}

/// The account's own device tokens. The list is fetched by `/assets/tokens.js`
/// so the page stays out of both browser bundles; the copy and revoke controls
/// use the same JSON contract native hosts use.
pub fn tokens_page(username: &str, is_super: bool) -> String {
    let admin = if is_super {
        "      <a class=\"menu-button\" href=\"/admin\">管理</a>\n"
    } else {
        ""
    };
    let menu = format!("      <a class=\"menu-button\" href=\"/\">返回对话</a>\n{admin}");
    let main = r#"  <main class="admin-main">
    <section class="admin-card">
      <h2 class="admin-title">设备令牌</h2>
      <p class="admin-hint">
        给 App、小程序这类没有 cookie 的宿主使用。明文只在签发时显示一次，
        之后只保留摘要；撤销后，下一次请求立即返回 401。
      </p>
      <form class="admin-form" id="token-create-form">
        <label class="admin-field">
          <span>名称</span>
          <input id="token-label" name="label" type="text" maxlength="64"
                 placeholder="例如：我的手机" autocomplete="off">
        </label>
        <button type="submit" class="admin-button">签发令牌</button>
      </form>
      <p class="admin-notice" id="token-notice" role="status" aria-live="polite" hidden></p>
      <div class="token-reveal" id="token-reveal" hidden>
        <p class="token-reveal-title">新令牌（只显示这一次）</p>
        <code class="token-value" id="token-value"></code>
        <button type="button" class="admin-button admin-button-quiet" id="token-copy">复制</button>
      </div>
    </section>

    <section class="admin-card">
      <h2 class="admin-title">有效令牌（<span id="token-count">0</span>）</h2>
      <table class="admin-table">
        <thead>
          <tr>
            <th>名称</th>
            <th>签发时间</th>
            <th>有效期</th>
            <th>最近使用</th>
            <th></th>
          </tr>
        </thead>
        <tbody id="token-list">
          <tr><td colspan="5" class="admin-empty">加载中…</td></tr>
        </tbody>
      </table>
    </section>
  </main>

  <dialog class="token-revoke-dialog" id="token-revoke-dialog" aria-labelledby="token-revoke-title">
    <h2 class="token-revoke-title" id="token-revoke-title">撤销这个令牌？</h2>
    <p class="token-revoke-target" id="token-revoke-target"></p>
    <p class="token-revoke-hint">撤销后，正在使用它的设备下一次请求就会返回 401。</p>
    <div class="token-revoke-actions">
      <button type="button" class="token-revoke-no" data-token-revoke-choice="cancel">取消</button>
      <button type="button" class="token-revoke-yes" data-token-revoke-choice="confirm">撤销</button>
    </div>
  </dialog>

  <script type="module" src="/assets/tokens.js"></script>
"#;
    page_shell("设备令牌", "admin-page token-page", &menu, username, main)
}
