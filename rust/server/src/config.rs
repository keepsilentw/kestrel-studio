use std::sync::OnceLock;

use url::Url;

const DEFAULT_BASE_URL: &str = "https://token-plan.cn-beijing.maas.aliyuncs.com";
const DEFAULT_CHAT_MODEL: &str = "deepseek-v4.1-flash";
const DEFAULT_VIDEO_MODEL: &str = "wan3.0-video";
const DEFAULT_VOICE_MODEL: &str = "qwen-audio-3.0-realtime-plus";
const DEFAULT_PUBLIC_BASE_URL: &str = "https://try.kestrel.justwork.link";
/// Per-account media budget. `0` disables the check; the default is a guard rail,
/// not a product limit, and can be overridden with `ASSET_QUOTA_BYTES`.
const DEFAULT_ASSET_QUOTA_BYTES: i64 = 2 * 1024 * 1024 * 1024;
/// Turns one account may have in flight at once. One conversation already allows
/// only one turn (409); this closes the "open N conversations" multiplication,
/// which is the remaining way to make N paid model calls at the same time. `0`
/// disables it. Low on purpose: every turn is billed, and a person who needs a
/// third simultaneous answer can wait for one to finish.
const DEFAULT_MAX_TURNS_PER_USER: u32 = 2;
const MAX_TURNS_PER_USER_CEILING: u32 = 16;
/// Generated assets one account may keep in a trailing 24-hour window. The burst
/// guard (turns in flight) is `MAX_TURNS_PER_USER`; this is the volume guard, so a
/// script that spaces its calls out still stops. `0` disables it. Generous on
/// purpose: the whole site had 23 assets after weeks when this was added.
const DEFAULT_DAILY_ASSET_LIMIT: u32 = 200;
const MAX_DAILY_ASSET_LIMIT: u32 = 10_000;
/// SQLite pool size. SQLite still serialises writers, so this buys concurrent
/// readers (pages, auth, asset lookups), not concurrent commits. Four is enough
/// for a 2-vCPU host with one public traffic source; the clamp is there because a
/// fat-fingered env value should not open a thousand connections.
const DEFAULT_DB_POOL_SIZE: u32 = 4;
const MAX_DB_POOL_SIZE: u32 = 32;
const DEFAULT_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::High;
const REASONING_EFFORTS: [ReasoningEffort; 4] = [
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
];

/// Reasoning depth sent to the Responses API. Defaults to `high`: measured
/// against the live endpoint, the provider default produced ~3 reasoning deltas
/// for an image request while `high` produced >1300, and the thinking panel is
/// only worth showing at the higher setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BailianConfig {
    pub base_url: String,
    pub chat_model: String,
    pub reasoning_effort: ReasoningEffort,
    /// One model covers both text-to-video and image-to-video: the render is
    /// image-to-video only when the request carries a `first_frame` media item,
    /// so there is nothing to pick between here.
    pub video_model: String,
    /// The realtime speech model behind /voice.
    pub voice_model: String,
}

/// Credentials for the bootstrap super admin. Both halves come from the
/// environment so the repository carries no working login for a deployed
/// instance.
#[derive(Debug, Clone)]
pub struct SeedAccount {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub port: u16,
    /// `NODE_ENV=production` (set by the Dockerfile). Gates the convenience
    /// account: a published repository must not describe a credential that also
    /// works against a deployed instance.
    pub is_production: bool,
    pub session_secret: String,
    pub database_file: String,
    pub storage_dir: String,
    /// Connections the SQLite pool may open. See [`DEFAULT_DB_POOL_SIZE`]: writers
    /// still serialise, this is for readers.
    pub db_pool_size: u32,
    /// In-flight turns one account may have. `0` disables the check.
    pub max_turns_per_user: u32,
    /// Generated assets per account per trailing 24 hours. `0` disables it.
    pub daily_asset_limit: u32,
    pub public_base_url: String,
    pub asset_quota_bytes: i64,
    /// See [`ClientBundle`]: which client the chat and voice pages load.
    pub client_bundle: ClientBundle,
    /// None when either `SUPER_ADMIN_*` variable is absent — then no super admin is seeded.
    pub super_admin: Option<SeedAccount>,
    pub bailian: BailianConfig,
}

fn read_env(key: &str) -> Option<String> {
    let value = std::env::var(key).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_port() -> u16 {
    read_env("PORT")
        .and_then(|raw| raw.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .unwrap_or(9949)
}

fn read_asset_quota() -> i64 {
    read_env("ASSET_QUOTA_BYTES")
        .and_then(|raw| raw.parse::<i64>().ok())
        .filter(|value| *value >= 0)
        .unwrap_or(DEFAULT_ASSET_QUOTA_BYTES)
}

/// `0` is meaningful (off), so unlike the pool size this does not clamp up to 1;
/// a negative or unparsable value falls back to the default instead.
fn read_max_turns_per_user() -> u32 {
    clamp_max_turns_per_user(read_env("MAX_TURNS_PER_USER").as_deref())
}

fn clamp_max_turns_per_user(raw: Option<&str>) -> u32 {
    raw.and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_MAX_TURNS_PER_USER)
        .min(MAX_TURNS_PER_USER_CEILING)
}

/// `0` is meaningful (off), so negative and unparsable values fall back to the
/// default instead of clamping to something the operator did not ask for.
fn read_daily_asset_limit() -> u32 {
    clamp_daily_asset_limit(read_env("DAILY_ASSET_LIMIT").as_deref())
}

fn clamp_daily_asset_limit(raw: Option<&str>) -> u32 {
    raw.and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_DAILY_ASSET_LIMIT)
        .min(MAX_DAILY_ASSET_LIMIT)
}

/// A nonsense `DB_POOL_SIZE` falls back to the default rather than refusing to
/// boot: unlike the client bundle, a wrong pool size is a performance setting, not
/// a "which code is that page running" decision.
fn read_db_pool_size() -> u32 {
    clamp_db_pool_size(read_env("DB_POOL_SIZE").as_deref())
}

fn clamp_db_pool_size(raw: Option<&str>) -> u32 {
    raw.and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_DB_POOL_SIZE)
        .clamp(1, MAX_DB_POOL_SIZE)
}

fn read_reasoning_effort() -> ReasoningEffort {
    let Some(raw) = read_env("BAILIAN_REASONING_EFFORT") else {
        return DEFAULT_REASONING_EFFORT;
    };
    let normalized = raw.trim().to_lowercase();
    REASONING_EFFORTS
        .into_iter()
        .find(|effort| effort.as_str() == normalized)
        .unwrap_or(DEFAULT_REASONING_EFFORT)
}

/// Which browser bundle the two pages load.
///
/// `nest` is what production has been running; `wasm` points `/` and `/voice` at the
/// Rust→WASM client under `public/wasm/` (roadmap.md §4.3 P3). The switch exists because
/// the WASM client has never been observed in a browser: flipping it is a grey release,
/// and the rollback has to be one environment variable rather than a redeploy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientBundle {
    Nest,
    Wasm,
}

impl ClientBundle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nest => "nest",
            Self::Wasm => "wasm",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "nest" => Some(Self::Nest),
            "wasm" => Some(Self::Wasm),
            _ => None,
        }
    }
}

/// An unknown value is a startup error, not a silent fallback to `nest`.
///
/// `BAILIAN_REASONING_EFFORT` does fall back silently, and that is fine: a wrong effort
/// changes how much thinking shows up. This one decides which client a page runs, so a
/// typo would hide a whole rollout — or, worse, a page would load a client nobody
/// intended to ship.
fn read_client_bundle() -> Result<ClientBundle, String> {
    let Some(raw) = read_env("CLIENT_BUNDLE") else {
        return Ok(ClientBundle::Nest);
    };
    ClientBundle::parse(&raw.to_lowercase())
        .ok_or_else(|| format!("CLIENT_BUNDLE must be nest or wasm, got {raw:?}"))
}

/// Fallback for a missing `SESSION_SECRET` outside production. The same value
/// signs sessions and asset frame URLs, so a published constant would let anyone
/// forge either. Generated once per process — the frame URL is signed in one
/// request and verified in another, so a per-call value would reject every
/// image-to-video frame. Restarting logs local sessions out, which is the price
/// of never shipping a guessable secret.
static GENERATED_SESSION_SECRET: OnceLock<String> = OnceLock::new();

fn read_session_secret(is_production: bool) -> Result<String, String> {
    if let Some(from_env) = read_env("SESSION_SECRET") {
        return Ok(from_env);
    }
    if is_production {
        return Err("SESSION_SECRET must be set when NODE_ENV=production".to_string());
    }
    Ok(GENERATED_SESSION_SECRET
        .get_or_init(|| hex::encode(&random_bytes(32)))
        .clone())
}

/// Both variables must be present, and neither may be blank: a half-configured
/// super admin is a locked-out /admin with no way to notice.
fn read_super_admin() -> Option<SeedAccount> {
    let username = read_env("SUPER_ADMIN_USERNAME")?;
    let password = read_env("SUPER_ADMIN_PASSWORD")?;
    Some(SeedAccount { username, password })
}

fn trim_trailing_slashes(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

impl AppConfig {
    pub fn load() -> Result<Self, String> {
        let is_production = read_env("NODE_ENV").as_deref() == Some("production");
        Ok(Self {
            port: read_port(),
            client_bundle: read_client_bundle()?,
            is_production,
            session_secret: read_session_secret(is_production)?,
            database_file: read_env("DATABASE_FILE")
                .unwrap_or_else(|| "data/kestrel-studio.db".to_string()),
            storage_dir: read_env("STORAGE_DIR").unwrap_or_else(|| "storage".to_string()),
            db_pool_size: read_db_pool_size(),
            max_turns_per_user: read_max_turns_per_user(),
            daily_asset_limit: read_daily_asset_limit(),
            public_base_url: trim_trailing_slashes(
                &read_env("PUBLIC_BASE_URL").unwrap_or_else(|| DEFAULT_PUBLIC_BASE_URL.to_string()),
            ),
            asset_quota_bytes: read_asset_quota(),
            super_admin: read_super_admin(),
            bailian: BailianConfig {
                base_url: trim_trailing_slashes(
                    &read_env("BAILIAN_BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
                ),
                chat_model: read_env("BAILIAN_CHAT_MODEL")
                    .unwrap_or_else(|| DEFAULT_CHAT_MODEL.to_string()),
                reasoning_effort: read_reasoning_effort(),
                video_model: read_env("BAILIAN_VIDEO_MODEL")
                    .unwrap_or_else(|| DEFAULT_VIDEO_MODEL.to_string()),
                voice_model: read_env("BAILIAN_VOICE_MODEL")
                    .unwrap_or_else(|| DEFAULT_VOICE_MODEL.to_string()),
            },
        })
    }

    /// The provider fetches the frame itself, so a base URL it cannot reach makes
    /// image-to-video impossible. Detected up front to give a real reason instead
    /// of a provider-side download error minutes later.
    pub fn is_publicly_reachable(&self) -> bool {
        match Url::parse(&self.public_base_url) {
            Ok(parsed) => matches!(
                parsed.host_str(),
                Some(host) if host != "localhost" && host != "127.0.0.1" && host != "::1"
            ),
            Err(_) => false,
        }
    }
}

/// Byte helpers shared by config, sessions and signing.
pub(crate) fn random_bytes(len: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut buf = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

pub(crate) mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
            out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
        }
        out
    }
}

#[cfg(test)]
impl AppConfig {
    /// A configuration a unit test owns. There used to be a process-wide
    /// `install()` here instead, which made every test that resolved an asset path
    /// or signed a frame URL depend on which other test had run first.
    pub fn test(storage_dir: &str, public_base_url: &str) -> Self {
        Self {
            port: 8848,
            client_bundle: ClientBundle::Nest,
            is_production: false,
            session_secret: "unit-test-secret".to_string(),
            database_file: String::new(),
            storage_dir: storage_dir.to_string(),
            db_pool_size: 1,
            max_turns_per_user: 2,
            daily_asset_limit: 200,
            public_base_url: public_base_url.to_string(),
            asset_quota_bytes: 0,
            super_admin: None,
            bailian: BailianConfig {
                base_url: "https://bailian.invalid".to_string(),
                chat_model: "chat-model".to_string(),
                reasoning_effort: ReasoningEffort::High,
                video_model: "video-model".to_string(),
                voice_model: "voice-model".to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_daily_asset_limit, clamp_db_pool_size, clamp_max_turns_per_user, ClientBundle,
        DEFAULT_DAILY_ASSET_LIMIT, DEFAULT_DB_POOL_SIZE, DEFAULT_MAX_TURNS_PER_USER,
        MAX_DAILY_ASSET_LIMIT, MAX_DB_POOL_SIZE, MAX_TURNS_PER_USER_CEILING,
    };

    /// `docker-compose.yml` writes this name, so the spelling is an interface: parse it
    /// back from the string it prints, and refuse everything else. A typo must not pick
    /// a client silently — that is the whole difference between this setting and
    /// `BAILIAN_REASONING_EFFORT`, which falls back because a wrong effort is cosmetic.
    #[test]
    fn the_client_bundle_round_trips_and_refuses_nonsense() {
        for bundle in [ClientBundle::Nest, ClientBundle::Wasm] {
            assert_eq!(ClientBundle::parse(bundle.as_str()), Some(bundle));
        }
        // `read_env` already trims and lowercases upstream; this is the value list.
        for raw in ["", "wasmb", "ts", "WAM", "n"] {
            assert_eq!(
                ClientBundle::parse(raw),
                None,
                "{raw:?} must not pick a client"
            );
        }
    }

    /// The pool size is a performance knob, so nonsense falls back instead of
    /// refusing to boot — but it must never leave the range the pool can honour.
    #[test]
    fn the_pool_size_falls_back_and_clamps() {
        assert_eq!(clamp_db_pool_size(None), DEFAULT_DB_POOL_SIZE);
        assert_eq!(clamp_db_pool_size(Some("8")), 8);
        assert_eq!(clamp_db_pool_size(Some(" 8 ")), 8, "trimmed");
        assert_eq!(clamp_db_pool_size(Some("0")), 1, "zero would deadlock");
        assert_eq!(clamp_db_pool_size(Some("999")), MAX_DB_POOL_SIZE);
        assert_eq!(clamp_db_pool_size(Some("-4")), DEFAULT_DB_POOL_SIZE);
        assert_eq!(clamp_db_pool_size(Some("many")), DEFAULT_DB_POOL_SIZE);
    }

    /// `0` must survive — it is the documented "off" value, not a typo.
    #[test]
    fn the_turn_cap_falls_back_clamps_and_keeps_zero() {
        assert_eq!(clamp_max_turns_per_user(None), DEFAULT_MAX_TURNS_PER_USER);
        assert_eq!(clamp_max_turns_per_user(Some("0")), 0, "off");
        assert_eq!(clamp_max_turns_per_user(Some("1")), 1);
        assert_eq!(
            clamp_max_turns_per_user(Some("99")),
            MAX_TURNS_PER_USER_CEILING
        );
        assert_eq!(
            clamp_max_turns_per_user(Some("-1")),
            DEFAULT_MAX_TURNS_PER_USER
        );
        assert_eq!(
            clamp_max_turns_per_user(Some("two")),
            DEFAULT_MAX_TURNS_PER_USER
        );
    }

    #[test]
    fn the_daily_asset_limit_falls_back_clamps_and_keeps_zero() {
        assert_eq!(clamp_daily_asset_limit(None), DEFAULT_DAILY_ASSET_LIMIT);
        assert_eq!(clamp_daily_asset_limit(Some("0")), 0, "off");
        assert_eq!(clamp_daily_asset_limit(Some("50")), 50);
        assert_eq!(
            clamp_daily_asset_limit(Some("999999")),
            MAX_DAILY_ASSET_LIMIT
        );
        assert_eq!(
            clamp_daily_asset_limit(Some("-1")),
            DEFAULT_DAILY_ASSET_LIMIT
        );
        assert_eq!(
            clamp_daily_asset_limit(Some("lots")),
            DEFAULT_DAILY_ASSET_LIMIT
        );
    }
}
