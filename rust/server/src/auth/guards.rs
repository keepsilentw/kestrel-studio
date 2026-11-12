use axum::extract::FromRequestParts;
use axum::http::header::{self, HeaderMap};
use axum::http::request::Parts;
use axum::http::Method;
use axum::response::{IntoResponse, Response};

use super::roles::Principal;
use super::roles::is_super_admin;
use super::session;
use super::service::AuthService;
use super::tokens;
use crate::error::AppError;
use crate::state::AppState;

/// Rebuilds the principal from the request. `Authorization: Bearer` wins over the
/// cookie; when both are present there is no identity merge, so an invalid Bearer
/// is anonymous rather than silently falling back to the cookie
/// (docs/roadmap.md §5.1). `None` also covers a bad signature, an expired or
/// unknown row, a revoked device token, and an account deleted since.
pub async fn principal_from(state: &AppState, headers: &HeaderMap) -> Option<Principal> {
    if let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        return principal_from_bearer(state, raw).await;
    }
    let raw = headers.get(header::COOKIE).and_then(|value| value.to_str().ok());
    principal_from_cookie(state, raw).await
}

/// Two kinds of credential arrive as a Bearer token: a device token minted by
/// `POST /api/v1/tokens` (stored hashed in `api_tokens`) and the one-shot session
/// token returned by `POST /api/v1/session` (the same sid the cookie carries, stored
/// in `sessions`). Device tokens are prefixed, so the two lookups cannot collide.
async fn principal_from_bearer(state: &AppState, header: &str) -> Option<Principal> {
    let token = bearer_token(header)?;
    let user_id = match tokens::resolve(&state.pool, token).await.ok()? {
        Some(user_id) => user_id,
        None => session::principal_id(&state.pool, token).await.ok()??,
    };
    AuthService::new(state.pool.clone())
        .find_by_id(user_id)
        .await
        .ok()
        .flatten()
}

fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Some(token)
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

#[cfg(test)]
mod tests {
    use super::bearer_token;

    #[test]
    fn the_bearer_scheme_is_case_insensitive_and_the_token_is_trimmed() {
        assert_eq!(bearer_token("Bearer kst_abc"), Some("kst_abc"));
        assert_eq!(bearer_token("bearer   kst_abc  "), Some("kst_abc"));
        assert_eq!(bearer_token("Basic kst_abc"), None);
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("kst_abc"), None);
    }
}
