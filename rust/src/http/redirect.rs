use axum::http::header::LOCATION;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// `express`'s `res.redirect()` answers **302**. axum's `Redirect::to` answers 303,
/// which is a different contract: the login form's failure path and the
/// view-guard bounce are both documented and tested as 302 (and a browser treats
/// 303 as "re-fetch with GET", which is not what the old server promised).
pub fn found(location: &str) -> Response {
    (
        StatusCode::FOUND,
        [(LOCATION, HeaderValue::from_str(location).expect("a location"))],
    )
        .into_response()
}
