use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::config::{hex, AppConfig};

/// Signed, expiring, session-free URL for one asset.
///
/// Image-to-video requires handing the provider a URL it can fetch on its own,
/// and the normal download endpoint is behind the session guard. Rather than
/// exposing assets, this signs a single asset id with an expiry so the link is
/// useless once it lapses and cannot be edited into a different id.
const FRAME_TTL_MS: i64 = 2 * 60 * 60 * 1000;

fn signature(asset_id: i64, expires_at: i64, secret: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(format!("{asset_id}.{expires_at}").as_bytes());
    hex::encode(&mac.finalize().into_bytes())
}

pub fn build_signed_frame_url(config: &AppConfig, asset_id: i64, now_ms: i64) -> String {
    let expires_at = now_ms + FRAME_TTL_MS;
    let sig = signature(asset_id, expires_at, &config.session_secret);
    format!(
        "{}/api/v1/assets/{asset_id}/frame?exp={expires_at}&sig={sig}",
        config.public_base_url
    )
}

pub fn verify_signed_frame(
    config: &AppConfig,
    asset_id: i64,
    expires_at: Option<i64>,
    provided: &str,
) -> bool {
    let Some(expires_at) = expires_at else {
        return false;
    };
    if expires_at < chrono::Utc::now().timestamp_millis() {
        return false;
    }
    let expected = signature(asset_id, expires_at, &config.session_secret);
    constant_time_eq(expected.as_bytes(), provided.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_round_trips_and_tampering_fails() {
        let config = AppConfig::test("/tmp/kestrel-signed-url", "https://example.test");
        let now = chrono::Utc::now().timestamp_millis();
        let url = build_signed_frame_url(&config, 42, now);
        assert!(url.starts_with("https://example.test/api/v1/assets/42/frame?"), "{url}");

        let query = url.split_once('?').expect("query").1;
        let mut exp = None;
        let mut sig = String::new();
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').expect("pair");
            match key {
                "exp" => exp = value.parse::<i64>().ok(),
                "sig" => sig = value.to_string(),
                _ => {}
            }
        }
        assert_eq!(exp, Some(now + FRAME_TTL_MS), "two hours");
        assert!(verify_signed_frame(&config, 42, exp, &sig));
        assert!(!verify_signed_frame(&config, 43, exp, &sig), "another asset id");
        let tampered = format!("{}{}", if sig.starts_with('f') { 'e' } else { 'f' }, &sig[1..]);
        assert!(!verify_signed_frame(&config, 42, exp, &tampered));
        assert!(!verify_signed_frame(&config, 42, None, &sig));
        let mut rotated = AppConfig::test("/tmp/x", "https://example.test");
        rotated.session_secret = "a-rotated-secret".to_string();
        assert!(
            !verify_signed_frame(&rotated, 42, exp, &sig),
            "a different session secret must not accept it"
        );
        assert!(!verify_signed_frame(
            &config,
            42,
            Some(now - 1),
            &signature(42, now - 1, &config.session_secret)
        ));
    }

    #[test]
    fn a_trailing_slash_in_the_base_url_is_not_double_printed() {
        let config = AppConfig::test("/tmp/x", "https://example.test");
        let url = build_signed_frame_url(&config, 1, 0);
        assert_eq!(url.matches("//").count(), 1, "only the scheme separator: {url}");
    }
}
