use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

/// Defence in depth for pages whose assistant text is model output.
///
/// `script-src` still allows inline code because the WASM loader is emitted
/// inline in the page shell (it must know which bundle `CLIENT_BUNDLE` selected).
/// The rest of the policy is what buys the protection: no external script or
/// connection target, no framing, no plugins, and forms may only post here.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
base-uri 'self'; \
form-action 'self'; \
frame-ancestors 'none'; \
object-src 'none'; \
img-src 'self' data: blob:; \
media-src 'self' blob:; \
connect-src 'self'; \
script-src 'self' 'unsafe-inline'; \
style-src 'self' 'unsafe-inline'";

/// Adds headers to every response, including the WebSocket upgrade. Only headers
/// are touched, so this is safe in front of the one route that hijacks the
/// connection (`docs/architecture.md` §13.3).
pub async fn apply(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), geolocation=(), microphone=(self)"),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    response
}
