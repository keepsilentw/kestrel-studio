use axum::extract::{Path as UrlPath, Query, State};
use axum::response::Response;
use std::collections::HashMap;

use crate::common::http::read_positive_int;
use crate::common::signed_url::verify_signed_frame;
use crate::error::AppError;
use crate::http::chat::serve_file;
use crate::state::AppState;

/// Session-free asset access for the provider.
///
/// Image-to-video hands the provider a URL it must fetch on its own, and the
/// normal download endpoint sits behind the session guard. The signed query string
/// is the authorization here, so this route is deliberately **not** guarded — an
/// HMAC over the asset id plus an expiry, which cannot be replayed after it lapses
/// or edited into a different asset.
pub async fn frame(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let Some(asset_id) = read_positive_int(Some(&id)) else {
        return Err(AppError::forbidden("forbidden"));
    };
    let expires_at = params
        .get("exp")
        .map(String::as_str)
        .and_then(|value| value.parse::<i64>().ok());
    let signature = params.get("sig").map(String::as_str).unwrap_or_default();
    if !verify_signed_frame(&state.config, asset_id, expires_at, signature) {
        return Err(AppError::forbidden("forbidden"));
    }

    let Some((file_path, mime)) = state.conversations.asset_file(asset_id).await? else {
        return Err(AppError::not_found("not found"));
    };
    // The provider is the only consumer and it fetches once: six hundred seconds
    // of private caching is enough, and the frame is not readable again after the
    // signature has expired anyway.
    serve_file(&file_path, &mime, None, "private, max-age=600").await
}
