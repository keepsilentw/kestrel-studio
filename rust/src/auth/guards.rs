use axum::extract::FromRequestParts;
use axum::http::header::{self, HeaderMap};
use axum::http::request::Parts;
use axum::http::Method;
use axum::response::{IntoResponse, Response};

use super::roles::Principal;
use super::roles::is_super_admin;
use super::session;
use super::service::AuthService;
use crate::error::AppError;
use crate::state::AppState;

/// Rebuilds the principal from the request cookie. `None` covers every way to be
/// anonymous: no cookie, a bad signature, an expired or unknown row, and a row
/// whose account has since been deleted.
pub async fn principal_from(state: &AppState, headers: &HeaderMap) -> Option<Principal> {
    let raw = headers.get(header::COOKIE).and_then(|value| value.to_str().ok());
    principal_from_cookie(state, raw).await
}

/// The same lookup from a raw `Cookie` header value, for the callers that have the
/// string rather than a `HeaderMap`.
pub async fn principal_from_cookie(state: &AppState, cookie: Option<&str>) -> Option<Principal> {
    let token = session::token_from_cookie(cookie?, &state.config.session_secret)?;
    let user_id = session::principal_id(&state.pool, &token).await.ok()??;
    AuthService::new(state.pool.clone())
        .find_by_id(user_id)
        .await
        .ok()
        .flatten()
}

/// For JSON endpoints: a missing session becomes 401.
pub struct Authed(pub Principal);

impl FromRequestParts<AppState> for Authed {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let headers = parts.headers.clone();
        principal_from(state, &headers)
            .await
            .map(Authed)
            .ok_or_else(|| AppError::unauthorized("未登录"))
    }
}

/// For HTML routes: a missing session becomes a redirect to the login page.
pub struct ViewAuthed(pub Principal);

impl FromRequestParts<AppState> for ViewAuthed {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let headers = parts.headers.clone();
        principal_from(state, &headers)
            .await
            .map(ViewAuthed)
            .ok_or_else(|| crate::http::redirect::found("/login"))
    }
}

/// Gate for the whole `/admin` surface.
///
/// Anonymous goes to the login page; a logged-in account without the privilege is
/// sent back to the chat page. POSTs get a 403 instead of that redirect — quietly
/// bouncing a rejected write to a page that looks unchanged would read as success.
pub struct SuperAdmin(pub Principal);

impl FromRequestParts<AppState> for SuperAdmin {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let Some(principal) = principal_from(state, &parts.headers).await else {
            return Err(crate::http::redirect::found("/login"));
        };
        if is_super_admin(Some(&principal)) {
            return Ok(SuperAdmin(principal));
        }
        if parts.method == Method::GET {
            return Err(crate::http::redirect::found("/"));
        }
        Err(AppError::forbidden("需要超级管理员权限").into_response())
    }
}
