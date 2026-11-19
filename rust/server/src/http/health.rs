use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;

/// Liveness plus the one dependency that makes every page useless when it is
/// gone. `SELECT 1` is deliberately cheap: the pool has a single connection, so a
/// hung or locked database shows up here without adding measurable traffic.
pub async fn health(State(state): State<AppState>) -> Response {
    match sqlx::query("SELECT 1").fetch_one(&state.pool).await {
        Ok(_) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(error) => {
            tracing::warn!(%error, "health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "ok": false })),
            )
                .into_response()
        }
    }
}
