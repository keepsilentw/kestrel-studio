use super::html::escape;
use super::render::RenderedMessage;

/// The super admin's pages.
///
/// Ported from `views/admin*.hbs`. One thing to keep in mind while reading: these
/// pages carry **no client script** — plain POST + redirect, like `/login` — so
/// everything a page needs has to be in the HTML the server prints.

pub struct Notice {
    pub text: &'static str,
    pub is_error: bool,
}

pub struct PresentedAccount {
    pub id: i64,
    pub username: String,
    pub role_label: String,
    pub created_at_label: String,
    pub conversation_count: i64,
    pub asset_count: i64,
}

pub struct ConversationRow {
    pub id: i64,
    pub title: String,
    pub owner_name: String,
    pub updated_at_label: String,
    /// Shown rather than filtered out: a soft-deleted conversation is one its
    /// owner hid, and reading it is the super admin's reason for having this page.
    pub deleted_at_label: Option<String>,
}

pub struct AccountChip {
    pub id: i64,
    pub username: String,
    pub is_active: bool,
}

fn shell(title: &str, menu: &[(&str, &str)], username: &str, main: &str) -> String {
    let links: String = menu
        .iter()
        .map(|(label, href)| format!("      <a class=\"menu-button\" href=\"{href}\">{label}</a>\n"))
        .collect();
    format!(
        "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>{title} · Kestrel Studio</title>\n  <link rel=\"icon\" type=\"image/svg+xml\" href=\"/logo.svg\">\n  <link rel=\"stylesheet\" href=\"/assets/main.css\">\n</head>\n<body class=\"admin-page\">\n  <header class=\"topbar\">\n    <a class=\"brand\" href=\"/\">\n      <img class=\"brand-logo\" src=\"/logo.svg\" alt=\"\" width=\"32\" height=\"32\">\n      <span class=\"brand-name\">Kestrel Studio</span>\n    </a>\n\n    <nav class=\"menu\">\n{links}    </nav>\n\n    <div class=\"account\">\n      <div class=\"account-menu\">\n        <span class=\"account-name\">{name}</span>\n        <form action=\"/logout\" method=\"post\" class=\"logout-form\">\n          <button type=\"submit\" class=\"logout-button\">退出登录</button>\n        </form>\n      </div>\n    </div>\n  </header>\n\n{main}</body>\n</html>\n",
        title = escape(title),
        links = links,
        name = escape(username),
        main = main,
    )
}

fn notice_html(notice: Option<&Notice>) -> String {
    match notice {
        None => String::new(),
        Some(note) => {
            let class = if note.is_error { " admin-notice-error" } else { "" };
            format!("    <p class=\"admin-notice{class}\">{}</p>\n", escape(note.text))
        }
    }
}

pub fn accounts_page(username: &str, accounts: &[PresentedAccount], notice: Option<&Notice>) -> String {
    let mut rows = String::new();
    for account in accounts {
        rows.push_str(&format!(
            "          <tr>\n            <td class=\"admin-account-name\">{name}</td>\n            <td>{role}</td>\n            <td><a href=\"/admin/conversations?user={id}\">{conversations}</a></td>\n            <td>{assets}</td>\n            <td>{created}</td>\n            <td><a href=\"/admin/users/{id}\">管理</a></td>\n          </tr>\n",
            name = escape(&account.username),
            role = escape(&account.role_label),
            id = account.id,
            conversations = account.conversation_count,
            assets = account.asset_count,
            created = escape(&account.created_at_label),
        ));
    }

    let main = format!(
        "  <main class=\"admin-main\">\n{note}    <section class=\"admin-card\">\n      <h2 class=\"admin-title\">新建账号</h2>\n      <p class=\"admin-hint\">\n        新账号一律是普通用户，只能看到自己的会话。用户名 3-32 位字母、数字、下划线或短横线，密码至少 6 位。\n      </p>\n      <form class=\"admin-form\" action=\"/admin/users\" method=\"post\">\n        <label class=\"admin-field\">\n          <span>用户名</span>\n          <input name=\"username\" type=\"text\" required autocomplete=\"username\">\n        </label>\n        <label class=\"admin-field\">\n          <span>密码</span>\n          <input name=\"password\" type=\"password\" required autocomplete=\"new-password\">\n        </label>\n        <button type=\"submit\" class=\"admin-button\">创建</button>\n      </form>\n    </section>\n\n    <section class=\"admin-card\">\n      <h2 class=\"admin-title\">账号（{count}）</h2>\n      <table class=\"admin-table\">\n        <thead>\n          <tr>\n            <th>账号</th>\n            <th>角色</th>\n            <th>会话</th>\n            <th>资产</th>\n            <th>创建时间</th>\n            <th></th>\n          </tr>\n        </thead>\n        <tbody>\n{rows}        </tbody>\n      </table>\n    </section>\n  </main>\n",
        note = notice_html(notice),
        count = accounts.len(),
        rows = rows,
    );
    shell("账号管理", &[("/", "返回对话"), ("/admin/conversations", "全站会话")], username, &main)
}

pub fn conversations_page(
    username: &str,
    rows: &[ConversationRow],
    chips: &[AccountChip],
    owner_name: Option<&str>,
    site_total: usize,
    notice: Option<&Notice>,
) -> String {
    let all_active = if owner_name.is_none() { " is-active" } else { "" };
    let mut chip_items = format!("        <li><a class=\"admin-chip{all_active}\" href=\"/admin/conversations\">全部</a></li>\n");
    for chip in chips {
        let active = if chip.is_active { " is-active" } else { "" };
        chip_items.push_str(&format!(
            "        <li>\n          <a class=\"admin-chip{active}\" href=\"/admin/conversations?user={}\">{}</a>\n        </li>\n",
            chip.id,
            escape(&chip.username)
        ));
    }

    let mut body = String::new();
    for row in rows {
        let deleted = match row.deleted_at_label.as_deref() {
            None => String::new(),
            Some(label) => format!("\n              <span class=\"admin-tag\">已删除 · {}</span>", escape(label)),
        };
        body.push_str(&format!(
            "          <tr>\n            <td>\n              {}{deleted}\n            </td>\n            <td>{owner}</td>\n            <td>{updated}</td>\n            <td><a href=\"/admin/conversations/{id}\">查看</a></td>\n          </tr>\n",
            escape(&row.title),
            owner = escape(&row.owner_name),
            updated = escape(&row.updated_at_label),
            id = row.id,
        ));
    }
    if body.is_empty() {
        body.push_str("          <tr><td colspan=\"4\" class=\"admin-empty\">没有会话</td></tr>\n");
    }

    let scoped = match owner_name {
        None => String::new(),
        Some(name) => format!("，当前只看 {} 的", escape(name)),
    };
    let main = format!(
        "  <main class=\"admin-main\">\n{note}    <section class=\"admin-card\">\n      <h2 class=\"admin-title\">全站会话</h2>\n      <p class=\"admin-hint\">\n        共 {total} 个会话{scoped}。查看他人会话是只读的。\n      </p>\n\n      <ul class=\"admin-chips\">\n{chips}      </ul>\n\n      <table class=\"admin-table\">\n        <thead>\n          <tr>\n            <th>会话</th>\n            <th>归属</th>\n            <th>更新时间</th>\n            <th></th>\n          </tr>\n        </thead>\n        <tbody>\n{rows}        </tbody>\n      </table>\n    </section>\n  </main>\n",
        note = notice_html(notice),
        total = site_total,
        scoped = scoped,
        chips = chip_items,
        rows = body,
    );
    shell("全站会话", &[("/", "返回对话"), ("/admin", "账号管理")], username, &main)
}

pub fn account_page(
    username: &str,
    account: &PresentedAccount,
    notice: Option<&Notice>,
) -> String {
    let main = format!(
        "  <main class=\"admin-main\">\n{note}    <section class=\"admin-card\">\n      <h2 class=\"admin-title\">{name}</h2>\n      <dl class=\"admin-facts\">\n        <dt>角色</dt><dd>{role}</dd>\n        <dt>会话</dt><dd><a href=\"/admin/conversations?user={id}\">{conversations} 个</a></dd>\n        <dt>资产</dt><dd>{assets} 个</dd>\n        <dt>创建时间</dt><dd>{created}</dd>\n      </dl>\n    </section>\n\n    <section class=\"admin-card\">\n      <h2 class=\"admin-title\">重置密码</h2>\n      <p class=\"admin-hint\">账号名只作显示，实际按本页地址里的账号重置。</p>\n\n      <form class=\"admin-form\" action=\"/admin/users/{id}/password\" method=\"post\">\n        <label class=\"admin-field\">\n          <span>账号</span>\n          <input name=\"username\" type=\"text\" value=\"{name}\" readonly autocomplete=\"username\">\n        </label>\n        <label class=\"admin-field\">\n          <span>新密码</span>\n          <input name=\"password\" type=\"password\" required autocomplete=\"new-password\">\n        </label>\n        <button type=\"submit\" class=\"admin-button\">重置密码</button>\n      </form>\n    </section>\n\n    <section class=\"admin-card admin-card-danger\">\n      <h2 class=\"admin-title\">删除账号</h2>\n      <p class=\"admin-hint\">\n        将一并删除该账号名下 {conversations} 个会话、{assets} 个资产文件，\n        以及这些会话的全部消息与任务记录。此操作不可撤销。\n      </p>\n      <a class=\"admin-button admin-button-danger\" href=\"/admin/users/{id}/delete\">删除账号</a>\n    </section>\n  </main>\n",
        note = notice_html(notice),
        name = escape(&account.username),
        role = escape(&account.role_label),
        id = account.id,
        conversations = account.conversation_count,
        assets = account.asset_count,
        created = escape(&account.created_at_label),
    );
    let menu = [("/", "返回对话"), ("/admin", "账号管理"), ("/admin/conversations", "全站会话")];
    shell(&account.username, &menu, username, &main)
}

pub fn delete_page(username: &str, account: &PresentedAccount) -> String {
    let main = format!(
        "  <main class=\"admin-main\"\n    <section class=\"admin-card admin-card-danger\">\n      <h2 class=\"admin-title\">删除账号 {name}</h2>\n      <p class=\"admin-hint\">\n        将一并删除该账号名下 {conversations} 个会话、{assets} 个资产文件，\n        以及这些会话的全部消息与任务记录。此操作不可撤销。\n      </p>\n      <form class=\"admin-form\" action=\"/admin/users/{id}/delete\" method=\"post\">\n        <button type=\"submit\" class=\"admin-button admin-button-danger\">确认删除</button>\n        <a class=\"admin-button admin-button-quiet\" href=\"/admin\">取消</a>\n      </form>\n    </section>\n  </main>\n",
        name = escape(&account.username),
        conversations = account.conversation_count,
        assets = account.asset_count,
        id = account.id,
    );
    shell("删除账号", &[("/", "返回对话"), ("/admin", "账号管理")], username, &main)
}

pub fn conversation_page(
    username: &str,
    title: &str,
    owner_name: &str,
    updated_at_label: &str,
    deleted_at_label: Option<&str>,
    is_own: bool,
    messages: &[RenderedMessage],
) -> String {
    let mut turns = String::new();
    for message in messages {
        turns.push_str(&super::templates::turn(message));
    }
    if turns.is_empty() {
        turns.push_str("      <p class=\"admin-empty\">这个会话还没有消息</p>\n");
    }
    let deleted = match deleted_at_label {
        None => String::new(),
        Some(label) => format!("\n      · <span class=\"admin-tag\">已于 {} 被用户删除</span>", escape(label)),
    };
    let scope = if is_own { "· 这是你自己的会话" } else { "· 只读视图，不能在此发送消息" };

    let main = format!(
        "  <main class=\"admin-main\"\n    <p class=\"admin-banner\">\n      {owner} 的会话 · 更新于 {updated}{deleted}\n      {scope}\n    </p>\n    <h2 class=\"admin-title\">{title}</h2>\n\n    <div class=\"stream admin-stream\">\n{turns}    </div>\n  </main>\n",
        owner = escape(owner_name),
        updated = escape(updated_at_label),
        deleted = deleted,
        scope = scope,
        title = escape(title),
        turns = turns,
    );
    shell(
        title,
        &[("/", "返回对话"), ("/admin", "账号管理"), ("/admin/conversations", "全站会话")],
        username,
        &main,
    )
}
