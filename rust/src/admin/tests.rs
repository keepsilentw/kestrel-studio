//! The admin surface: who gets in, what the forms accept, and what a removal
//! actually takes with it.

use axum::http::StatusCode;
use serde_json::json;
use sqlx::Row;
use tower::ServiceExt;

use crate::auth::password;
use crate::state::AppState;
use crate::auth::roles::Role;
use crate::media::StoredAsset;
use crate::task::TaskKind;
use crate::test_utils::*;

/// The acting super admin, as `(cookie, id)`.
async fn boss(state: &AppState) -> (String, i64) {
    account(state, "boss", Role::Super).await
}

#[tokio::test]
async fn only_a_super_admin_gets_through_and_a_rejected_post_says_403() {
    let (app, state) = app().await;
    let (user_cookie, _) = account(&state, "walker", Role::User).await;
    let (admin_cookie, admin_id) = boss(&state).await;

    // Anonymous: to the login page, like every other HTML route.
    let anonymous = app.clone().oneshot(get("/admin", None)).await.expect("anonymous");
    assert_eq!(anonymous.status(), StatusCode::FOUND);
    assert_eq!(anonymous.headers()["location"], "/login");

    // A signed-in normal account on a GET: bounced to the chat page, not told why.
    let bounced = app
        .clone()
        .oneshot(get("/admin", Some(&user_cookie)))
        .await
        .expect("not privileged");
    assert_eq!(bounced.headers()["location"], "/");

    // ...while a rejected *write* answers 403. Quietly bouncing a POST to a page
    // that looks unchanged would read as success.
    let forbidden = app
        .clone()
        .oneshot(form_post(
            "/admin/users",
            &user_cookie,
            &[("username", "sneaky"), ("password", "hunter2")],
        ))
        .await
        .expect("forbidden post");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(forbidden).await["message"],
        json!("需要超级管理员权限")
    );
    assert_eq!(
        sqlx::query("SELECT COUNT(*) AS n FROM users WHERE username = 'sneaky'")
            .fetch_one(&state.pool)
            .await
            .expect("count")
            .try_get::<i64, _>("n")
            .expect("n"),
        0,
        "the form did not get past the guard"
    );

    let page = text_body(app.oneshot(get("/admin", Some(&admin_cookie))).await.expect("admin")).await;
    assert!(page.contains("账号管理"));
    assert!(page.contains("<span class=\"account-name\">boss</span>"));
}

#[tokio::test]
async fn the_create_form_validates_then_lands_the_account_as_a_plain_user() {
    let (app, state) = app().await;
    let (admin_cookie, admin_id) = boss(&state).await;

    for (username, password, expected) in [
        ("ab", "hunter2", "invalid"),
        ("has space", "hunter2", "invalid"),
        ("newbie", "12345", "invalid"),
        ("newbie", "hunter2", "created"),
        ("newbie", "hunter2", "duplicate"),
    ] {
        let response = app
            .clone()
            .oneshot(form_post(
                "/admin/users",
                &admin_cookie,
                &[("username", username), ("password", password)],
            ))
            .await
            .expect("create");
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers()["location"],
            format!("/admin?notice={expected}").as_str(),
            "{username}/{expected}"
        );
    }

    let page = text_body(app.oneshot(get("/admin", Some(&admin_cookie))).await.expect("list")).await;
    assert!(page.contains("class=\"admin-account-name\">newbie</td>"), "{page}");
    assert!(page.contains("<td>普通用户</td>"), "new accounts are never privileged");
    // Both halves of the password rule are spelled out where the form is.
    assert!(page.contains("用户名 3-32 位字母、数字、下划线或短横线，密码至少 6 位"));
}

#[tokio::test]
async fn the_password_form_shows_the_account_it_belongs_to() {
    let (app, state) = app().await;
    let (admin_cookie, admin_id) = boss(&state).await;
    let (_target_cookie, target_id) = account(&state, "walker", Role::User).await;

    let page = text_body(
        app.clone()
            .oneshot(get(&format!("/admin/users/{target_id}"), Some(&admin_cookie)))
            .await
            .expect("account page"),
    )
    .await;

    // Chrome warns about a password box with no visible username next to it, and
    // password managers need the pairing to know whose password this is. So the
    // name is a real, visible, readonly field in the same form - and the id, not
    // the value, is what identifies the account.
    let form = page
        .split("<form class=\"admin-form\" action=\"/admin/users/")
        .nth(1)
        .expect("the reset form")
        .split("</form>")
        .next()
        .expect("the form body");
    assert!(form.starts_with(&format!("{target_id}/password\"")), "{form}");
    let username_at = form.find("name=\"username\"").expect("a username field");
    let password_at = form.find("name=\"password\"").expect("a password field");
    assert!(username_at < password_at, "the name is shown before the secret");
    assert!(form.contains("value=\"walker\""), "{form}");
    assert!(form.contains("readonly"), "{form}");
    assert!(page.contains("账号名只作显示，实际按本页地址里的账号重置。"));
}

#[tokio::test]
async fn resetting_a_password_rehashes_only_the_target_account() {
    let (app, state) = app().await;
    let (admin_cookie, admin_id) = boss(&state).await;
    let (_target_cookie, target_id) = account(&state, "walker", Role::User).await;
    let before = stored_hash(&state, "walker").await;

    let rejected = app
        .clone()
        .oneshot(form_post(
            &format!("/admin/users/{target_id}/password"),
            &admin_cookie,
            &[("password", "short")],
        ))
        .await
        .expect("too short");
    assert_eq!(rejected.headers()["location"], format!("/admin/users/{target_id}?notice=invalid"));
    assert_eq!(stored_hash(&state, "walker").await, before, "unchanged");

    let accepted = app
        .clone()
        .oneshot(form_post(
            &format!("/admin/users/{target_id}/password"),
            &admin_cookie,
            &[("password", "a-new-secret")],
        ))
        .await
        .expect("reset");
    assert_eq!(
        accepted.headers()["location"],
        format!("/admin/users/{target_id}?notice=password-reset")
    );
    let after = stored_hash(&state, "walker").await;
    assert!(password::verify("a-new-secret", &after), "the new one works");
    assert!(!password::verify("secret", &after), "and the old one does not");

    // A password is hashed as typed: trimming it would silently change it.
    let spaced = app
        .clone()
        .oneshot(form_post(
            &format!("/admin/users/{target_id}/password"),
            &admin_cookie,
            &[("password", "  padded-secret  ")],
        ))
        .await
        .expect("padded");
    assert_eq!(spaced.headers()["location"], format!("/admin/users/{target_id}?notice=password-reset"));
    assert!(password::verify("  padded-secret  ", &stored_hash(&state, "walker").await));

    let gone = app
        .clone()
        .oneshot(form_post(
            "/admin/users/999999/password",
            &admin_cookie,
            &[("password", "whatever1")],
        ))
        .await
        .expect("missing");
    assert_eq!(gone.headers()["location"], "/admin?notice=missing");
}

#[tokio::test]
async fn removing_an_account_takes_its_rows_and_its_files() {
    let (app, state) = app().await;
    let (admin_cookie, admin_id) = boss(&state).await;
    let (_, target_id) = account(&state, "walker", Role::User).await;

    // Give the account something to lose: a conversation, a message, an asset and
    // the mirrored bytes.
    let conversation_id = state.conversations.create(target_id, "图").await.expect("conversation");
    let message_id = state
        .conversations
        .append_assistant_message(conversation_id, "好了", None)
        .await
        .expect("message");
    // The row keeps a bare name, so the file has to live in the storage dir this
    // instance resolves against - that is what the cascade deletes.
    let storage = state.config.storage_dir.clone();
    std::fs::create_dir_all(&storage).expect("storage");
    let file = format!("{storage}/frame.png");
    std::fs::write(&file, b"1234").expect("bytes");
    state
        .conversations
        .add_asset(
            message_id,
            &StoredAsset {
                kind: TaskKind::Image,
                file_path: file.clone(),
                source_url: None,
                mime: "image/png".to_string(),
                bytes: 4,
            },
        )
        .await
        .expect("asset");

    // The confirm page states the cost before the form exists.
    let confirm = text_body(
        app.clone()
            .oneshot(get(&format!("/admin/users/{target_id}/delete"), Some(&admin_cookie)))
            .await
            .expect("confirm"),
    )
    .await;
    assert!(confirm.contains("删除账号 walker"), "{confirm}");
    assert!(confirm.contains("1 个会话、1 个资产文件"), "{confirm}");
    assert!(confirm.contains("此操作不可撤销"), "{confirm}");

    // Refused cases first: the acting account, and the last super admin.
    let self_removal = app
        .clone()
        .oneshot(form_post(&format!("/admin/users/{admin_id}/delete"), &admin_cookie, &[]))
        .await
        .expect("self");
    assert_eq!(self_removal.headers()["location"], "/admin?notice=self");

    // The `last-super` guard cannot be reached through this surface: the gate
    // requires the actor to be a super admin, so the sole super admin is always
    // the actor and `self` answers first. The Nest version behaves the same way;
    // the branch is kept for a future role-editing path. What is reachable is the
    // ordinary case - with two super admins, either can go while one remains.
    let created = app
        .clone()
        .oneshot(form_post("/admin/users", &admin_cookie, &[("username", "boss2"), ("password", "hunter2")]))
        .await
        .expect("second account");
    assert_eq!(created.headers()["location"], "/admin?notice=created");
    let second_id: i64 = sqlx::query("SELECT id FROM users WHERE username = 'boss2'")
        .fetch_one(&state.pool)
        .await
        .expect("boss2")
        .try_get("id")
        .expect("id");
    sqlx::query("UPDATE users SET role = 'super' WHERE id = ?1")
        .bind(second_id)
        .execute(&state.pool)
        .await
        .expect("promote");
    let removed_other = app
        .clone()
        .oneshot(form_post(&format!("/admin/users/{second_id}/delete"), &admin_cookie, &[]))
        .await
        .expect("remove the other super admin");
    assert_eq!(removed_other.headers()["location"], "/admin?notice=removed");
    assert_eq!(super_count(&state).await, 1, "the actor is still the only super admin");


    let removed = app
        .clone()
        .oneshot(form_post(&format!("/admin/users/{target_id}/delete"), &admin_cookie, &[]))
        .await
        .expect("remove");
    assert_eq!(removed.headers()["location"], "/admin?notice=removed");

    for (table, sql) in [
        ("users", "SELECT COUNT(*) AS n FROM users WHERE id = ?1"),
        ("conversations", "SELECT COUNT(*) AS n FROM conversations WHERE user_id = ?1"),
        ("messages", "SELECT COUNT(*) AS n FROM messages WHERE conversation_id = ?1"),
        ("assets", "SELECT COUNT(*) AS n FROM assets WHERE message_id = ?1"),
    ] {
        let n: i64 = sqlx::query(sql)
            .bind(if table == "users" || table == "conversations" { target_id } else { message_id })
            .fetch_one(&state.pool)
            .await
            .unwrap_or_else(|error| panic!("{table}: {error}"))
            .try_get("n")
            .expect("count");
        assert_eq!(n, 0, "{table} rows survived");
    }
    assert!(!std::path::Path::new(&file).exists(), "the mirrored bytes go too");
}

#[tokio::test]
async fn the_site_wide_conversation_view_is_read_only() {
    let (app, state) = app().await;
    let (admin_cookie, admin_id) = boss(&state).await;
    let (_, walker_id) = account(&state, "walker", Role::User).await;

    let conversation_id = state.conversations.create(walker_id, "红隼俯冲").await.expect("conversation");
    state
        .conversations
        .append_user_message(conversation_id, "红隼俯冲", None)
        .await
        .expect("message");
    let other = state.conversations.create(walker_id, "另一条").await.expect("conversation");
    state.conversations.soft_delete_conversations(&[other]).await.expect("hide");

    let list = text_body(app.clone().oneshot(get("/admin/conversations", Some(&admin_cookie))).await.expect("list")).await;
    assert!(list.contains("共 2 个会话"), "{list}");
    assert!(list.contains("查看他人会话是只读的"), "{list}");
    assert!(list.contains("已删除 ·"), "a hidden conversation is shown, not filtered out: {list}");
    assert!(list.contains(&format!("/admin/conversations?user={walker_id}")), "the row links to its owner: {list}");

    // Narrowed to one account, and the chip that selects it reads as active.
    let scoped = text_body(
        app.clone()
            .oneshot(get(&format!("/admin/conversations?user={walker_id}"), Some(&admin_cookie)))
            .await
            .expect("scoped"),
    )
    .await;
    assert!(scoped.contains("当前只看 walker 的"), "{scoped}");
    assert_eq!(scoped.matches("admin-chip is-active").count(), 1, "exactly one active chip");

    let page = text_body(
        app.clone()
            .oneshot(get(&format!("/admin/conversations/{conversation_id}"), Some(&admin_cookie)))
            .await
            .expect("conversation"),
    )
    .await;
    assert!(page.contains("walker 的会话 · 更新于"), "{page}");
    assert!(page.contains("只读视图，不能在此发送消息"), "{page}");
    // Nothing here talks to /api: no composer, no client script.
    assert!(!page.contains("id=\"composer-form\""), "{page}");
    assert!(!page.contains("/assets/main.js"), "{page}");
    assert!(page.contains("data-message-id=\"1\""), "the turn is rendered by the server: {page}");

    // The admin's own conversation reads differently, and a bad id is bounced.
    let own = state.conversations.create(state.conversations.owner_of(conversation_id).await.expect("owner").expect("owner"), "自留").await.expect("own");
    let own_page = text_body(app.clone().oneshot(get(&format!("/admin/conversations/{own}"), Some(&admin_cookie))).await.expect("own")).await;
    assert!(own_page.contains("这是你自己的会话") || own_page.contains("只读视图"), "{own_page}");

    let bad = app.oneshot(get("/admin/conversations/not-a-number", Some(&admin_cookie))).await.expect("bad");
    assert_eq!(bad.headers()["location"], "/admin/conversations?notice=missing");
}

async fn super_count(state: &AppState) -> i64 {
    sqlx::query("SELECT COUNT(*) AS n FROM users WHERE role = 'super'")
        .fetch_one(&state.pool)
        .await
        .expect("count")
        .try_get("n")
        .expect("n")
}

async fn stored_hash(state: &AppState, username: &str) -> String {
    let row = sqlx::query("SELECT password_hash FROM users WHERE username = ?1")
        .bind(username)
        .fetch_one(&state.pool)
        .await
        .expect("row");
    sqlx::Row::try_get::<String, _>(&row, "password_hash").expect("hash")
}
