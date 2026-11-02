use axum::extract::State;
use axum::http::header::{HeaderMap, SET_COOKIE};
use axum::http::HeaderValue;
use axum::response::Response;
use axum::Form;
use serde::Deserialize;

use super::guards;
use super::session;
use super::service::AuthService;
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
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let user = AuthService::new(state.pool.clone())
        .validate(&form.username, &form.password)
        .await?;
    let Some(user) = user else {
        return Ok(crate::http::redirect::found("/login?error=1"));
    };

    let token = session::create(&state.pool, user.id).await?;
    let mut response = crate::http::redirect::found("/");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session::set_cookie(&token, &state.config.session_secret))
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
        HeaderValue::from_str(&session::cleared_cookie()).expect("static cookie value"),
    );
    Ok(response)
}

/// Whether this request already carries a live session. The `/login` page uses
/// it to bounce a signed-in user back to the chat.
pub async fn signed_in(state: &AppState, headers: &HeaderMap) -> bool {
    guards::principal_from(state, headers).await.is_some()
}
