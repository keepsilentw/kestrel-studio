use std::path::PathBuf;

use sqlx::Connection;
use sqlx::Row;

const DEFAULT_PROVIDER_ID: &str = "bailian-token-plan";

fn provider_id() -> String {
    non_empty(std::env::var("BAILIAN_PROVIDER_ID").ok().as_deref())
        .unwrap_or_else(|| DEFAULT_PROVIDER_ID.to_string())
}

fn non_empty(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| dirs_fallback())
}

/// `node:os.homedir()` has no direct equivalent in std. The env vars above cover
/// every normal case; this only handles a process started without a HOME.
fn dirs_fallback() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

/// Read the token from cc-switch's own database.
///
/// This is the durable source: switching Codex to another provider rewrites
/// config.toml wholesale and drops the bailian-token-plan block, but the provider
/// row — and therefore the key — survives in cc-switch's database.
///
/// The Nest version pulled the value out with `json_extract`; here the column is
/// read as text and decoded by serde instead, so the lookup does not depend on
/// the SQLite build having the JSON1 extension.
async fn token_from_cc_switch_db() -> Option<String> {
    // A missing key must stay a "nothing found", not a panic: in a container with
    // no HOME the default path simply does not exist.
    let path = match non_empty(std::env::var("CC_SWITCH_DB").ok().as_deref()) {
        Some(explicit) => explicit,
        None => match home_dir() {
            Some(home) => home.join(".cc-switch").join("cc-switch.db").to_string_lossy().into_owned(),
            None => return None,
        },
    };
    if !std::path::Path::new(&path).exists() {
        return None;
    }

    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .read_only(true)
        .create_if_missing(false);
    let mut connection = sqlx::sqlite::SqliteConnection::connect_with(&options).await.ok()?;

    let row = sqlx::query("SELECT settings_config FROM providers WHERE id = ?1 AND app_type = 'codex'")
        .bind(provider_id())
        .fetch_optional(&mut connection)
        .await
        .ok()?;
    let raw: String = row?.try_get("settings_config").ok()?;

    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    non_empty(
        parsed
            .get("auth")?
            .get("OPENAI_API_KEY")?
            .as_str(),
    )
}

/// Fallback: read the bearer token cc-switch wrote into the Codex provider block
/// of `~/.codex/config.toml`.
fn token_from_codex_config() -> Option<String> {
    let path = home_dir()?.join(".codex").join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    token_from_config_text(&text, &provider_id())
}

/// Scans the provider's own table out of the TOML text and takes its
/// `experimental_bearer_token`. Split out so a test can drive it without touching
/// the developer's real config.
fn token_from_config_text(text: &str, id: &str) -> Option<String> {
    const KEY: &str = "experimental_bearer_token";
    let header = format!("[model_providers.{id}]");
    let start = text.find(&header)? + header.len();
    let rest = &text[start..];
    let block = match rest.find("\n[") {
        Some(next) => &rest[..next],
        None => rest,
    };
    let marker = block.find(KEY)?;
    let after = &block[marker + KEY.len()..];
    let quoted = after.find('"')? + 1;
    let value = &after[quoted..];
    let end = value.find('"')?;
    non_empty(Some(&value[..end]))
}

/// Precedence: `BAILIAN_API_KEY` / `DASHSCOPE_API_KEY`, then the cc-switch
/// database, then the Codex provider block.
///
/// The Nest version threw when nothing matched, which took the process down at
/// boot. Here the absence is a value: the server still serves pages and the
/// generation paths answer 503 with the same sentence, so a machine without a
/// key is diagnosable rather than unbootable.
pub async fn resolve_key() -> Option<String> {
    if let Some(from_env) = non_empty(std::env::var("BAILIAN_API_KEY").ok().as_deref())
        .or_else(|| non_empty(std::env::var("DASHSCOPE_API_KEY").ok().as_deref()))
    {
        return Some(from_env);
    }
    if let Some(from_db) = token_from_cc_switch_db().await {
        return Some(from_db);
    }
    token_from_codex_config()
}

pub const MISSING_KEY_MESSAGE: &str = "No Bailian API key found. Set BAILIAN_API_KEY, or keep the bailian-token-plan provider in cc-switch.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toml_block_scan_finds_only_that_providers_token() {
        let text = r#"
model = "gpt"

[model_providers.other]
experimental_bearer_token = "not-this-one"

[model_providers.bailian-token-plan]
base_url = "https://example.test"
experimental_bearer_token  =  "found-me"

[model_providers.third]
experimental_bearer_token = "nor-this"
"#;
        assert_eq!(
            token_from_config_text(text, "bailian-token-plan").as_deref(),
            Some("found-me")
        );
        assert_eq!(token_from_config_text(text, "missing-provider"), None);
        assert_eq!(
            token_from_config_text("[model_providers.x]\nother = 1\n", "x"),
            None,
            "a block without a token"
        );
    }

    #[test]
    fn blank_values_are_not_credentials() {
        assert_eq!(non_empty(Some("  ")), None);
        assert_eq!(non_empty(None), None);
        assert_eq!(non_empty(Some(" sk ")).as_deref(), Some("sk"));
    }
}
