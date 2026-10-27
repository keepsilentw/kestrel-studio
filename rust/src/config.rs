use std::sync::OnceLock;

use url::Url;

const DEFAULT_BASE_URL: &str = "https://token-plan.cn-beijing.maas.aliyuncs.com";
const DEFAULT_CHAT_MODEL: &str = "deepseek-v4.1-flash";
const DEFAULT_T2V_MODEL: &str = "happyhorse-1.1-t2v";
const DEFAULT_I2V_MODEL: &str = "happyhorse-1.1-i2v";
const DEFAULT_VOICE_MODEL: &str = "qwen-audio-3.0-realtime-plus";
const DEFAULT_PUBLIC_BASE_URL: &str = "https://try.kestrel.justwork.link";
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
    pub video_model_t2v: String,
    pub video_model_i2v: String,
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
    pub public_base_url: String,
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
        .unwrap_or(8848)
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
            is_production,
            session_secret: read_session_secret(is_production)?,
            database_file: read_env("DATABASE_FILE")
                .unwrap_or_else(|| "data/kestrel-studio.db".to_string()),
            storage_dir: read_env("STORAGE_DIR").unwrap_or_else(|| "storage".to_string()),
            public_base_url: trim_trailing_slashes(
                &read_env("PUBLIC_BASE_URL").unwrap_or_else(|| DEFAULT_PUBLIC_BASE_URL.to_string()),
            ),
            super_admin: read_super_admin(),
            bailian: BailianConfig {
                base_url: trim_trailing_slashes(
                    &read_env("BAILIAN_BASE_URL")
                        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
                ),
                chat_model: read_env("BAILIAN_CHAT_MODEL")
                    .unwrap_or_else(|| DEFAULT_CHAT_MODEL.to_string()),
                reasoning_effort: read_reasoning_effort(),
                video_model_t2v: read_env("BAILIAN_VIDEO_MODEL_T2V")
                    .unwrap_or_else(|| DEFAULT_T2V_MODEL.to_string()),
                video_model_i2v: read_env("BAILIAN_VIDEO_MODEL_I2V")
                    .unwrap_or_else(|| DEFAULT_I2V_MODEL.to_string()),
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

    pub fn decode(value: &str) -> Option<Vec<u8>> {
        let bytes = value.as_bytes();
        if bytes.len() % 2 != 0 {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks(2) {
            let hi = char::from(pair[0]).to_digit(16)?;
            let lo = char::from(pair[1]).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
        }
        Some(out)
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
            is_production: false,
            session_secret: "unit-test-secret".to_string(),
            database_file: String::new(),
            storage_dir: storage_dir.to_string(),
            public_base_url: public_base_url.to_string(),
            super_admin: None,
            bailian: BailianConfig {
                base_url: "https://bailian.invalid".to_string(),
                chat_model: "chat-model".to_string(),
                reasoning_effort: ReasoningEffort::High,
                video_model_t2v: "t2v-model".to_string(),
                video_model_i2v: "i2v-model".to_string(),
                voice_model: "voice-model".to_string(),
            },
        }
    }
}
