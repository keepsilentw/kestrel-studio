//! Shared fixtures for route-level tests.
//!
//! The router is driven with `tower::ServiceExt::oneshot` instead of a bound
//! socket: binding one is denied by the sandbox this project is developed in, and
//! a test that needs a port is a test that cannot run on a shared box. The
//! assertions are the same ones the Nest suite made over TCP - status codes,
//! headers, cookies and bodies.

use axum::http::{Request, Response};
use std::time::Duration;

use axum::body::{to_bytes, Body};

use futures_util::StreamExt;
use serde_json::{json, Value};
use sqlx::Row;
use tower::ServiceExt;

use crate::auth::password;
use crate::auth::roles::Role;
use crate::auth::session;
use crate::config::AppConfig;
use crate::state::AppState;

pub(crate) use axum::http::header::CONTENT_TYPE;

pub(crate) const LIMIT: usize = 4 << 20;

/// A JSON POST, for the endpoints that read a body rather than a form.
pub(crate) fn post_json_value(path: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    post_json(path, cookie, body)
}

/// A router over an in-memory database, plus the state behind it.
pub(crate) async fn app() -> (axum::Router, AppState) {
    let pool = crate::db::open_in_memory().await;
    let config = AppConfig::test("/tmp/kestrel-http-tests", "https://try.kestrel.test");
    let state = AppState::new(config, pool, Some("test-key".to_string()));
    (crate::http::router::build(state.clone()), state)
}

/// Creates an account and returns `(cookie header value, principal id)`.
pub(crate) async fn account(state: &AppState, username: &str, role: Role) -> (String, i64) {
    sqlx::query("INSERT INTO users (username, password_hash, role, created_at) VALUES (?1, ?2, ?3, 1)")
        .bind(username)
        .bind(password::hash("secret"))
        .bind(role.as_str())
        .execute(&state.pool)
        .await
        .expect("insert user");
    let id: i64 = sqlx::query("SELECT id FROM users WHERE username = ?1")
        .bind(username)
        .fetch_one(&state.pool)
        .await
        .expect("read back")
        .try_get("id")
        .expect("id");
    let token = session::create(&state.pool, id).await.expect("session");
    (
        format!(
            "connect.sid={}",
            session::cookie_value(&token, &state.config.session_secret)
        ),
        id,
    )
}

pub(crate) fn form_post(path: &str, cookie: &str, fields: &[(&str, &str)]) -> Request<Body> {
    let encoded = fields
        .iter()
        .map(|(key, value)| format!("{key}={}", value.replace(' ', "+")))
        .collect::<Vec<_>>()
        .join("&");
    Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "localhost")
        .header("cookie", cookie)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(encoded))
        .expect("request")
}

/// A conversation the caller owns, created through the API.
pub(crate) async fn new_conversation(app: &axum::Router, _state: &AppState, cookie: &str) -> i64 {
    let response = app
        .clone()
        .oneshot(post_json("/api/v1/conversations", Some(cookie), json!({ "prompt": "一只红隼" })))
        .await
        .expect("create");
    json_body(response).await["conversationId"].as_i64().expect("id")
}

pub(crate) fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(path)
        .header("host", "localhost");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::empty()).expect("request")
}

pub(crate) fn post_json(path: &str, cookie: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "localhost")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

pub(crate) fn post_form(path: &str, form: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "localhost")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form.to_string()))
        .expect("request")
}

pub(crate) async fn json_body(response: Response<Body>) -> Value {
    let bytes = to_bytes(response.into_body(), LIMIT)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

pub(crate) async fn text_body(response: Response<Body>) -> String {
    let bytes = to_bytes(response.into_body(), LIMIT).await.expect("body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Pulls a bounded number of chunks off a stream body. The conversation stream
/// never ends on its own, so a test reads what arrived and stops.
pub(crate) async fn peek(mut response: Response<Body>, chunks: usize) -> String {
    let mut out = String::new();
    // `into_data_stream` because axum's Body speaks `http_body::Body`, not
    // `futures_util::Stream`, until it is handed over as data.
    let mut body = response.into_body().into_data_stream();
    for _ in 0..chunks {
        match tokio::time::timeout(Duration::from_millis(500), body.next()).await {
            Ok(Some(Ok(bytes))) => out.push_str(&String::from_utf8_lossy(&bytes)),
            // A closed stream or a slow turn both end the peek: what arrived is
            // what the test asserts on.
            Ok(Some(Err(error))) => panic!("stream error: {error}"),
            Ok(None) | Err(_) => break,
        }
    }
    out
}
