//! Route-level checks driven through the router itself.
//!
//! The Nest suite booted a real listener and sent requests over TCP. Here the
//! router is driven with `tower::ServiceExt::oneshot` instead: same assertions on
//! status codes, headers and bodies, without a socket - binding one is denied by
//! the sandbox this project is developed in, and a test that needs a port is a
//! test that cannot run in CI on a shared box.
//!
//! The fixtures live in `crate::test_utils`, which the admin route tests share.

use crate::task::TaskKind;
use axum::body::to_bytes;
use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::agent::mode::Mode;
use crate::auth::roles::{Principal, Role};
use crate::conversation::ConversationEvent;
use crate::media::StoredAsset;
use sqlx::Row;
use crate::test_utils::*;

#[tokio::test]
async fn the_first_paint_is_ssr_only_and_the_guard_bounces_anonymously() {
    let (app, state) = app().await;
    let (cookie, owner) = account(&state, "walker", Role::User).await;

    // Anonymous: the HTML surface redirects instead of answering 401 like the API.
    let bounced = app.clone().oneshot(get("/", None)).await.expect("anonymous root");
    assert_eq!(bounced.status(), StatusCode::FOUND);
    assert_eq!(bounced.headers()["location"], "/login");

    // The login page renders for an anonymous visitor and says nothing about
    // credentials unless the redirect came back with ?error=1.
    let page = app.clone().oneshot(get("/login", None)).await.expect("login page");
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()["content-type"], "text/html; charset=utf-8");
    let body = text_body(page).await;
    assert!(body.contains("action=\"/login\" method=\"post\""), "{body}");
    assert!(!body.contains("用户名或密码错误"));
    let failed = text_body(app.clone().oneshot(get("/login?error=1", None)).await.expect("error page")).await;
    assert!(failed.contains("用户名或密码错误"));

    // A signed-in visitor on /login is bounced to the chat, not shown the form.
    let already = app.clone().oneshot(get("/login", Some(&cookie))).await.expect("signed in");
    assert_eq!(already.status(), StatusCode::FOUND);
    assert_eq!(already.headers()["location"], "/");

    // The chat page: shell, sidebar, and nothing painted for a conversation that
    // has no finished rows.
    let conversation_id = state.conversations.create(owner, "画一只红隼").await.expect("create");
    state
        .conversations
        .append_user_message(conversation_id, "画一只红隼", Some(Mode::Image))
        .await
        .expect("user row");
    let placeholder = state
        .conversations
        .create_assistant_placeholder(conversation_id, Some(Mode::Image))
        .await
        .expect("placeholder");

    let rendered = app
        .clone()
        .oneshot(get("/", Some(&cookie)))
        .await
        .expect("chat page");
    assert_eq!(rendered.status(), StatusCode::OK);
    let body = text_body(rendered).await;
    assert!(body.contains(&format!("data-conversation-id=\"{conversation_id}\"")), "{body}");
    assert!(body.contains("class=\"conversation-link is-active\""), "{body}");
    assert!(
        !body.contains(&format!("data-message-id=\"{placeholder}\"")),
        "an unfinished assistant row is not painted: the stream replays it"
    );
    assert!(body.contains("data-message-id=\"1\""), "the user row is painted");
    assert!(body.contains("<script type=\"module\" src=\"/assets/main.js\"></script>"));
    assert!(body.starts_with("<!DOCTYPE html>"), "the page is a document, not a fragment");

    // A `?c=` that belongs to somebody else is ignored rather than honoured, and
    // the page falls back to the caller's own most recent conversation.
    let (stranger_cookie, _) = account(&state, "nosy", Role::User).await;
    let hijacked = app
        .clone()
        .oneshot(get(&format!("/?c={conversation_id}"), Some(&stranger_cookie)))
        .await
        .expect("other account");
    let body = text_body(hijacked).await;
    assert!(body.contains("data-conversation-id=\"\""), "nothing to show: {body}");
    assert!(!body.contains("conversation-link is-active"), "{body}");
}

#[tokio::test]
async fn pending_tasks_and_the_catch_up_cursor_are_painted() {
    let (app, state) = app().await;
    let (cookie, owner) = account(&state, "worker", Role::User).await;
    let conversation_id = state.conversations.create(owner, "拍一段视频").await.expect("create");
    let message_id = state
        .conversations
        .append_user_message(conversation_id, "拍一段视频", Some(Mode::Video))
        .await
        .expect("user row");
    state
        .tasks
        .create(&crate::task::service::NewTask {
            conversation_id,
            message_id: Some(message_id),
            kind: TaskKind::Video,
            provider_task_id: "provider-1".to_string(),
            model: "t2v-model".to_string(),
            prompt: "红隼俯冲".to_string(),
            params: json!({ "duration": 5 }),
        })
        .await
        .expect("task");

    let body = text_body(app.clone().oneshot(get("/", Some(&cookie))).await.expect("page")).await;
    // The announcement event is journaled, so the cursor the page paints from is 1.
    assert!(body.contains("data-start-seq=\"1\""), "{body}");
    assert!(body.contains("class=\"task-pending\" data-task-id=\"1\""), "{body}");
    assert!(body.contains("视频任务 #1 排队中…"), "{body}");

    // Once the task is terminal it is no longer a pending row.
    state
        .tasks
        .finish(1, crate::task::TaskStatus::Succeeded, None, None, None)
        .await
        .expect("finish");
    let body = text_body(app.clone().oneshot(get("/", Some(&cookie))).await.expect("page")).await;
    assert!(!body.contains("task-pending"), "{body}");
}

#[tokio::test]
async fn anonymous_api_calls_are_rejected_and_the_login_pair_redirects() {
    let (app, state) = app().await;
    let (cookie, _) = account(&state, "walker", Role::User).await;

    let anonymous = app.clone().oneshot(get("/api/v1/conversations", None)).await.expect("call");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_body(anonymous).await["message"], json!("未登录"));

    // A forged cookie is a different thing from no cookie, and answered the same.
    let forged = app
        .clone()
        .oneshot(get("/api/v1/conversations", Some("connect.sid=s:ghost.deadbeef")))
        .await
        .expect("forged");
    assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);

    // Login is an HTML form: both outcomes are redirects, not JSON.
    let signed_in = app
        .clone()
        .oneshot(post_form("/login", "username=walker&password=secret"))
        .await
        .expect("login");
    assert_eq!(signed_in.status(), StatusCode::FOUND);
    assert_eq!(signed_in.headers()["location"], "/");
    let issued = signed_in.headers()["set-cookie"].to_str().expect("cookie").to_string();
    assert!(issued.starts_with("connect.sid=s:"), "{issued}");
    assert!(issued.contains("HttpOnly") && issued.contains("SameSite=Lax"), "{issued}");
    assert!(issued.contains("Max-Age=604800"), "seven days: {issued}");

    let rejected = app
        .clone()
        .oneshot(post_form("/login", "username=walker&password=wrong"))
        .await
        .expect("bad login");
    assert_eq!(rejected.status(), StatusCode::FOUND);
    assert_eq!(rejected.headers()["location"], "/login?error=1");

    // A missing password field is an empty string, not a 400 - the express body
    // parser produced the same thing.
    let partial = app
        .clone()
        .oneshot(post_form("/login", "username=walker"))
        .await
        .expect("partial");
    assert_eq!(partial.headers()["location"], "/login?error=1");

    let listed = app
        .clone()
        .oneshot(get("/api/v1/conversations", Some(&cookie)))
        .await
        .expect("list");
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(json_body(listed).await, json!([]));

    let out = app.oneshot(post_form("/logout", "")).await.expect("logout");
    assert_eq!(out.status(), StatusCode::FOUND);
    assert_eq!(out.headers()["location"], "/login");
    assert!(out.headers()["set-cookie"].to_str().expect("cleared").contains("Max-Age=0"));
}

#[tokio::test]
async fn turn_submission_answers_201_400_404_and_409() {
    let (app, state) = app().await;
    let (cookie, owner) = account(&state, "owner", Role::User).await;
    let (admin_cookie, _) = account(&state, "boss", Role::Super).await;

    let created = app
        .clone()
        .oneshot(post_json("/api/v1/conversations", Some(&cookie), json!({ "prompt": "画一只红隼" })))
        .await
        .expect("create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let conversation_id = json_body(created).await["conversationId"]
        .as_i64()
        .expect("id");
    assert!(conversation_id > 0);

    let empty = app
        .clone()
        .oneshot(post_json(
            &format!("/api/v1/conversations/{conversation_id}/turns"),
            Some(&cookie),
            json!({ "prompt": "   " }),
        ))
        .await
        .expect("empty");
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(empty).await["message"], json!("prompt 不能为空"));

    // An id that is not the caller's, or not a number, is "not found": inventing a
    // fresh conversation would let a stale `?c=` write a turn elsewhere.
    for path in [
        "/api/v1/conversations/999999/turns".to_string(),
        "/api/v1/conversations/abc/turns".to_string(),
    ] {
        let response = app
            .clone()
            .oneshot(post_json(&path, Some(&cookie), json!({ "prompt": "x", "mode": "image" })))
            .await
            .expect("missing");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }

    // A super admin may read a normal account's conversation but never post into
    // one, so it reads as not found here too.
    let as_admin = app
        .clone()
        .oneshot(post_json(
            &format!("/api/v1/conversations/{conversation_id}/turns"),
            Some(&admin_cookie),
            json!({ "prompt": "替我画" }),
        ))
        .await
        .expect("admin post");
    assert_eq!(as_admin.status(), StatusCode::NOT_FOUND);

    // The busy guard is exercised without a provider: holding the slot is exactly
    // what a running turn does, and a second submit would double-bill.
    assert!(state.claim_turn(conversation_id), "take the slot");
    let busy = app
        .clone()
        .oneshot(post_json(
            &format!("/api/v1/conversations/{conversation_id}/turns"),
            Some(&cookie),
            json!({ "prompt": "再来一次", "mode": "nonsense" }),
        ))
        .await
        .expect("busy");
    assert_eq!(busy.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(busy).await["message"], json!("该会话已有轮次在进行中"));
    state.release_turn(conversation_id);

    // Delete: 403 for a super admin (a rule about the caller, said out loud)...
    let forbidden = app
        .clone()
        .oneshot(post_json(
            &format!("/api/v1/conversations/{conversation_id}/delete"),
            Some(&admin_cookie),
            json!({}),
        ))
        .await
        .expect("forbidden");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(forbidden).await["message"], json!("超级管理员不提供会话删除"));

    // ...404 for somebody else's conversation...
    let (stranger_cookie, _) = account(&state, "stranger", Role::User).await;
    let nope = app
        .clone()
        .oneshot(post_json(
            &format!("/api/v1/conversations/{conversation_id}/delete"),
            Some(&stranger_cookie),
            json!({}),
        ))
        .await
        .expect("not yours");
    assert_eq!(nope.status(), StatusCode::NOT_FOUND);

    // ...and a hide for the owner: gone from the list, rows intact.
    let deleted = app
        .clone()
        .oneshot(post_json(
            &format!("/api/v1/conversations/{conversation_id}/delete"),
            Some(&cookie),
            json!({}),
        ))
        .await
        .expect("delete");
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(json_body(deleted).await, json!({ "ok": true }));

    let listed = app
        .clone()
        .oneshot(get("/api/v1/conversations", Some(&cookie)))
        .await
        .expect("list");
    assert_eq!(json_body(listed).await, json!([]), "hidden from its owner");
    // The owner can no longer read it - that is the whole point of a hide - while
    // the messages underneath survive, which is what makes it not a delete.
    assert!(
        !state
            .conversations
            .can_read(
                conversation_id,
                &Principal { id: owner, username: "owner".to_string(), role: Role::User }
            )
            .await
            .expect("read"),
        "hidden from its owner"
    );
    assert_eq!(
        sqlx::query("SELECT COUNT(*) AS n FROM conversations WHERE id = ?1")
            .bind(conversation_id)
            .fetch_one(&state.pool)
            .await
            .expect("count")
            .try_get::<i64, _>("n")
            .expect("count"),
        1,
        "the row is still there"
    );
    assert!(state
        .conversations
        .list_all()
        .await
        .expect("admin list")
        .iter()
        .any(|item| item.id == conversation_id && item.deleted_at.is_some()));

    // Messages: unreadable reads as an empty list rather than leaking the id.
    let foreign = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/conversations/{conversation_id}/messages"),
            Some(&stranger_cookie),
        ))
        .await
        .expect("foreign");
    assert_eq!(foreign.status(), StatusCode::OK);
    assert_eq!(json_body(foreign).await, json!([]));

    let tasks = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/conversations/{conversation_id}/tasks"),
            Some(&cookie),
        ))
        .await
        .expect("tasks");
    assert_eq!(json_body(tasks).await, json!([]));
}

#[tokio::test]
async fn the_stream_replays_the_journal_and_reports_a_gap_as_resync() {
    let (app, state) = app().await;
    let (cookie, _) = account(&state, "viewer", Role::User).await;
    let conversation_id = state.conversations.create(1, "hi").await.expect("create");

    state
        .events
        .publish(conversation_id, ConversationEvent::Text { message_id: 1, delta: "第一段".into() })
        .await
        .expect("publish");
    state
        .events
        .publish(conversation_id, ConversationEvent::TurnDone { message_id: 1 })
        .await
        .expect("publish");

    let path = format!("/api/v1/conversations/{conversation_id}/stream?from=0");
    let body = peek(app.clone().oneshot(get(&path, Some(&cookie))).await.expect("stream"), 4).await;
    assert!(body.contains("event: connected"), "{body}");
    // A connection-scoped frame must not move the client's cursor.
    let connected_at = body.find("event: connected").expect("connected");
    assert!(!body[..connected_at].contains("id:"), "{body}");
    assert!(body.contains("id: 1\nevent: text\ndata: {\"type\":\"text\""), "{body}");
    assert!(body.contains("第一段"), "the payload is the journaled JSON: {body}");
    assert!(body.contains("id: 2\nevent: turn_done"), "{body}");
    assert!(!body.contains("event: resync"), "from=0 is replayable: {body}");

    // The replayed frame is the journal's own bytes, not a re-serialization: field
    // order included, so a frame a client saw live is identical after a reconnect
    // even once the event shape has moved on under an old row.
    let stored: String = sqlx::query(
        "SELECT payload AS value FROM conversation_events WHERE conversation_id = ?1 AND seq = 1",
    )
    .bind(conversation_id)
    .fetch_one(&state.pool)
    .await
    .expect("journal row")
    .try_get("value")
    .expect("payload");
    assert!(
        body.contains(&format!("data: {stored}\n\n")),
        "verbatim replay: stored={stored} streamed={body}"
    );

    // Last-Event-ID outranks ?from: a reconnect must not see seq 1 twice.
    let mut request = get(&path, Some(&cookie));
    request.headers_mut().insert("last-event-id", "1".parse().expect("header"));
    let resumed = peek(app.clone().oneshot(request).await.expect("resume"), 3).await;
    assert!(!resumed.contains("id: 1\nevent: text"), "already seen: {resumed}");
    assert!(resumed.contains("id: 2\nevent: turn_done"), "{resumed}");

    // A cursor below the retained window is a gap: one resync frame, and no
    // partial replay that would look complete. Pruning is what moves the window,
    // so that is what the fixture does - adding a later row would leave seq 1 in
    // place and the cursor honest.
    sqlx::query("DELETE FROM conversation_events WHERE conversation_id = ?1 AND seq < 40")
        .bind(conversation_id)
        .execute(&state.pool)
        .await
        .expect("prune");
    sqlx::query(
        "INSERT INTO conversation_events (conversation_id, seq, message_id, type, payload, created_at) VALUES (?1, 40, 1, 'text', '{\"type\":\"text\",\"messageId\":1,\"delta\":\"x\"}', 1)",
    )
    .bind(conversation_id)
    .execute(&state.pool)
    .await
    .expect("retain a later event");
    let gapped_path = format!("/api/v1/conversations/{conversation_id}/stream?from=1");
    let gapped = peek(app.clone().oneshot(get(&gapped_path, Some(&cookie))).await.expect("gap"), 4).await;
    assert!(gapped.contains("event: resync"), "{gapped}");
    assert!(gapped.contains("\"cursor\":1"), "{gapped}");
    assert!(gapped.find("event: resync") < gapped.find("event: connected"), "resync comes first");
    assert!(gapped.contains("id: 40"), "the retained tail is still delivered: {gapped}");
    assert!(!gapped.contains("id: 1\nevent: text"), "the gap is not padded: {gapped}");

    // Someone else's conversation reads as not found, and an id that is not a
    // number is refused the same way.
    let (stranger_cookie, _) = account(&state, "nosy", Role::User).await;
    let denied = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/conversations/{conversation_id}/stream"),
            Some(&stranger_cookie),
        ))
        .await
        .expect("denied");
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    let anonymous = app
        .clone()
        .oneshot(get(&gapped_path, None))
        .await
        .expect("anonymous");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn assets_follow_ownership_and_the_signature_is_the_only_authority() {
    let (app, state) = app().await;
    let (cookie, owner) = account(&state, "holder", Role::User).await;
    let (stranger_cookie, _) = account(&state, "nosy", Role::User).await;

    std::fs::create_dir_all("/tmp/kestrel-http-tests").expect("storage dir");
    std::fs::write("/tmp/kestrel-http-tests/frame.png", b"1234").expect("asset bytes");
    let stored = StoredAsset {
        kind: TaskKind::Image,
        file_path: "/tmp/kestrel-http-tests/frame.png".to_string(),
        source_url: None,
        mime: "image/png".to_string(),
        bytes: 4,
    };
    let conversation_id = state.conversations.create(owner, "图").await.expect("create");
    let message_id = state
        .conversations
        .append_assistant_message(conversation_id, "好了", Some(Mode::Image))
        .await
        .expect("message");
    let asset_id = state
        .conversations
        .add_asset(message_id, &stored)
        .await
        .expect("asset");

    let owned = app
        .clone()
        .oneshot(get(&format!("/api/v1/assets/{asset_id}/download"), Some(&cookie)))
        .await
        .expect("download");
    assert_eq!(owned.status(), StatusCode::OK);
    assert_eq!(owned.headers()["content-type"], "image/png");
    assert_eq!(owned.headers()["content-length"], "4");
    assert!(owned.headers()["content-disposition"]
        .to_str()
        .expect("cd")
        .contains("frame.png"));
    let bytes = to_bytes(owned.into_body(), LIMIT).await.expect("bytes");
    assert_eq!(&bytes[..], b"1234");

    // A row whose bytes are gone is a 404, not a truncated 200.
    let ghost = state
        .conversations
        .add_asset(
            message_id,
            &StoredAsset {
                file_path: "/tmp/kestrel-http-tests/deleted.png".to_string(),
                ..stored.clone()
            },
        )
        .await
        .expect("ghost asset");
    let missing = app
        .clone()
        .oneshot(get(&format!("/api/v1/assets/{ghost}/download"), Some(&cookie)))
        .await
        .expect("missing");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let anonymous = app
        .clone()
        .oneshot(get(&format!("/api/v1/assets/{asset_id}/download"), None))
        .await
        .expect("anonymous");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let stranger = app
        .clone()
        .oneshot(get(&format!("/api/v1/assets/{asset_id}/download"), Some(&stranger_cookie)))
        .await
        .expect("stranger");
    assert_eq!(stranger.status(), StatusCode::NOT_FOUND, "not the owner's asset");

    // The frame route is deliberately outside the session guard: the signature is
    // the whole authorization, covering the asset id and the expiry.
    let unsigned = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/assets/{asset_id}/frame?exp=99999999999999&sig=deadbeef"),
            None,
        ))
        .await
        .expect("unsigned");
    assert_eq!(unsigned.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(unsigned).await["message"], json!("forbidden"));

    let expired = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/assets/{asset_id}/frame?exp=1000&sig={}", "00".repeat(32)),
            None,
        ))
        .await
        .expect("expired");
    assert_eq!(expired.status(), StatusCode::FORBIDDEN, "the expiry is checked");

    let url = crate::common::signed_url::build_signed_frame_url(
        &state.config,
        asset_id,
        chrono::Utc::now().timestamp_millis(),
    );
    let query = url.split_once('?').expect("query").1.to_string();
    let signed = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/assets/{asset_id}/frame?{query}"),
            None,
        ))
        .await
        .expect("signed");
    assert_eq!(signed.status(), StatusCode::OK);
    assert_eq!(signed.headers()["cache-control"], "private, max-age=600");
    assert_eq!(&to_bytes(signed.into_body(), LIMIT).await.expect("bytes")[..], b"1234");

    // The same signature on a different asset id does not work.
    let swapped = app
        .clone()
        .oneshot(get(&format!("/api/v1/assets/{ghost}/frame?{query}"), None))
        .await
        .expect("swapped");
    assert_eq!(swapped.status(), StatusCode::FORBIDDEN, "the signature covers the id");
}
