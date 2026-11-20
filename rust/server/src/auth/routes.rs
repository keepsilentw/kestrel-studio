use axum::extract::Path as UrlPath;
use axum::extract::State;
use axum::http::header::{HeaderMap, SET_COOKIE};
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use serde::Deserialize;
use serde_json::json;

use super::guards::Authed;
use super::service::AuthService;
use super::session;
use super::tokens;
use crate::common::http::read_positive_int;
use crate::error::AppError;
use crate::state::AppState;

/// The credential fields arrive from an HTML form, so a missing one is an empty
/// string rather than a 400, exactly like the express body parser produced.
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// Login is an HTML form post, so a failed attempt returns to the page with an
/// error flag instead of surfacing a bare 401 to the browser.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers);
    throttle(&state, &ip, &form.username)?;

    let user = AuthService::new(state.pool.clone())
        .validate(&form.username, &form.password)
        .await?;
    let Some(user) = user else {
        state.login_limiter.record_failure(&ip, &form.username);
        return Ok(crate::http::redirect::found("/login?error=1"));
    };
    state.login_limiter.record_success(&ip, &form.username);

    let token = session::create(&state.pool, user.id).await?;
    let mut response = crate::http::redirect::found("/");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session::set_cookie(
            &token,
            &state.config.session_secret,
            state.config.is_production,
        ))
        .map_err(|error| AppError::internal(anyhow::anyhow!(error)))?,
    );
    Ok(response)
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(raw) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(token) = session::token_from_cookie(raw, &state.config.session_secret) {
            session::destroy(&state.pool, &token).await?;
        }
    }

    let mut response = crate::http::redirect::found("/login");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session::cleared_cookie(state.config.is_production))
            .expect("static cookie value"),
    );
    Ok(response)
}

/// The JSON twin of the HTML login form, for hosts that have no cookie jar.
///
/// It returns the same session sid the cookie carries, once. Bearer resolution
/// accepts that sid directly (`auth::guards`), so a native host can use it right
/// away and later exchange it for a revocable device token through
/// `POST /api/v1/tokens`.
#[derive(Debug, Deserialize)]
pub struct SessionLogin {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

pub async fn json_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SessionLogin>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers);
    throttle(&state, &ip, &body.username)?;

    let user = AuthService::new(state.pool.clone())
        .validate(&body.username, &body.password)
        .await?;
    let Some(user) = user else {
        state.login_limiter.record_failure(&ip, &body.username);
        return Err(AppError::unauthorized("用户名或密码错误"));
    };
    state.login_limiter.record_success(&ip, &body.username);

    let token = session::create(&state.pool, user.id).await?;
    Ok(Json(json!({ "token": token, "userId": user.id })).into_response())
}

/// The address used for throttling. openresty is the only public entry point and
/// is expected to set one of these headers; a direct connection falls back to a
/// single shared bucket, which still bounds the damage from the host itself.
fn client_ip(headers: &HeaderMap) -> String {
    let raw = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(',').next())
        })
        .map(str::trim)
        .filter(|value| !value.is_empty());
    raw.map(|value| value.chars().take(64).collect())
        .unwrap_or_else(|| "direct".to_string())
}

fn throttle(state: &AppState, ip: &str, username: &str) -> Result<(), AppError> {
    let Some(seconds) = state.login_limiter.blocked_for(ip, username) else {
        return Ok(());
    };
    tracing::warn!(ip, "login throttled");
    Err(AppError::too_many_requests(format!(
        "登录尝试过于频繁，请 {seconds} 秒后再试"
    )))
}

#[derive(Debug, Deserialize)]
pub struct NewToken {
    #[serde(default)]
    pub label: String,
}

/// Mints a device token. The plaintext appears in this response and nowhere else;
/// the row keeps only its SHA-256 digest.
pub async fn create_token(
    Authed(user): Authed,
    State(state): State<AppState>,
    Json(body): Json<NewToken>,
) -> Result<Response, AppError> {
    let issued = tokens::issue(
        &state.pool,
        user.id,
        &body.label,
        state.config.token_ttl_days,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": issued.id,
            "label": tokens::normalize_label(&body.label),
            "token": issued.token,
            "createdAt": issued.created_at,
            "expiresAt": issued.expires_at,
        })),
    )
        .into_response())
}

pub async fn list_tokens(
    Authed(user): Authed,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let rows = tokens::list(&state.pool, user.id).await?;
    Ok(Json(json!(rows)).into_response())
}

pub async fn revoke_token(
    Authed(user): Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, AppError> {
    let Some(id) = read_positive_int(Some(&id)) else {
        return Err(AppError::not_found("not found"));
    };
    if !tokens::revoke(&state.pool, user.id, id).await? {
        return Err(AppError::not_found("not found"));
    }
    Ok(Json(json!({ "ok": true })).into_response())
}
