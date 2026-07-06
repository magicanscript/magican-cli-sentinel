/// Application-wide configuration.
///
/// All parameters are read from environment variables (or from `.env` via `dotenvy`).
/// There are no CLI flags for settings — only env vars, which makes it easy to run
/// the daemon under systemd or Docker with environment-based configuration.
use std::env;
use std::time::Duration;

use crate::error::SentinelError;

/// Main configuration structure.
/// Created once at startup via `Config::from_env()` and then passed by reference
/// to all modules that need it.
#[derive(Debug, Clone)]
pub struct Config {
    // --- Solana RPC ---
    /// URL of the node being monitored (required).
    /// Example: "http://192.168.1.10:8899"
    pub target_rpc_url: String,

    /// URL of the reference node used for slot comparison.
    /// Defaults to the official mainnet-beta endpoint.
    pub reference_rpc_url: String,

    // --- Polling parameters ---
    /// Interval between node polls.
    /// Read from `SENTINEL_POLL_INTERVAL_SECS`, default 10 seconds.
    pub poll_interval: Duration,

    // --- Alert thresholds ---
    /// Maximum allowed slot lag of the target node behind the reference.
    /// If `reference_slot - target_slot > slot_lag_threshold` → alert.
    pub slot_lag_threshold: u64,

    /// Maximum allowed response time of the target node in milliseconds.
    /// If RTT > rtt_threshold_ms → alert.
    pub rtt_threshold_ms: u64,

    /// Minimum interval between two consecutive alerts (cooldown).
    /// Prevents Telegram spam during a prolonged incident.
    pub alert_cooldown: Duration,

    // --- LLM ---
    /// API ключ для генерации текста. Необязательное поле.
    /// Читается из `LLM_API_KEY`; при отсутствии — пустая строка, и заголовок
    /// `Authorization` не отправляется (как ожидает локальная Ollama).
    pub llm_api_key: String,

    /// Модель для генерации текста.
    /// Читается из `LLM_MODEL`, по умолчанию "mistral-small-latest".
    pub llm_model: String,

    /// Базовый URL OpenAI-совместимого API (root, без `/chat/completions`).
    /// Читается из `LLM_BASE_URL`, по умолчанию "https://api.mistral.ai/v1".
    /// Смена значения переключает провайдера (Ollama/OpenAI/Groq/OpenRouter).
    pub llm_base_url: String,

    // --- Telegram ---
    /// Telegram bot token (format: "123456:ABC-DEF..."). Required.
    pub telegram_bot_token: String,

    /// Chat or channel ID to send alerts to (e.g. "-1001234567890"). Required.
    pub telegram_chat_id: String,
}

impl Config {
    /// Reads all parameters from environment variables.
    ///
    /// Required variables: `SOLANA_TARGET_RPC_URL`,
    /// `TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHAT_ID`.
    /// `LLM_API_KEY` is optional (empty → no `Authorization` header, for Ollama).
    /// If any of them is absent — returns `Err(SentinelError::Config)`.
    ///
    /// All other variables are optional and have sensible defaults.
    pub fn from_env() -> Result<Self, SentinelError> {
        Ok(Self {
            // Required fields: use the helper require_var() which returns a descriptive
            // error message if the variable is missing.
            target_rpc_url: require_var("SOLANA_TARGET_RPC_URL")?,

            // Optional fields: use unwrap_or_else() to supply defaults.
            reference_rpc_url: env::var("SOLANA_REFERENCE_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),

            poll_interval: Duration::from_secs(parse_u64_var("SENTINEL_POLL_INTERVAL_SECS", 10)?),

            slot_lag_threshold: parse_u64_var("SENTINEL_SLOT_LAG_THRESHOLD", 5)?,

            rtt_threshold_ms: parse_u64_var("SENTINEL_RTT_THRESHOLD_MS", 500)?,

            alert_cooldown: Duration::from_secs(parse_u64_var(
                "SENTINEL_ALERT_COOLDOWN_SECS",
                300,
            )?),

            llm_api_key: env::var("LLM_API_KEY").unwrap_or_default(),

            llm_model: env::var("LLM_MODEL").unwrap_or_else(|_| "mistral-small-latest".to_string()),

            llm_base_url: env::var("LLM_BASE_URL")
                .unwrap_or_else(|_| "https://api.mistral.ai/v1".to_string()),

            telegram_bot_token: require_var("TELEGRAM_BOT_TOKEN")?,

            telegram_chat_id: require_var("TELEGRAM_CHAT_ID")?,
        })
    }

    /// Returns a human-readable summary of the current configuration for startup logging.
    /// API keys are masked — only the first 8 characters are shown.
    pub fn summary(&self) -> String {
        format!(
            "target={} reference={} interval={:?} slot_lag_threshold={} rtt_threshold_ms={}ms cooldown={:?} model={} base_url={}",
            self.target_rpc_url,
            self.reference_rpc_url,
            self.poll_interval,
            self.slot_lag_threshold,
            self.rtt_threshold_ms,
            self.alert_cooldown,
            self.llm_model,
            self.llm_base_url,
        )
    }
}

/// Reads a required env variable.
/// Returns `SentinelError::Config` with the variable name if it is not set.
fn require_var(name: &str) -> Result<String, SentinelError> {
    env::var(name).map_err(|_| {
        SentinelError::Config(format!(
            "обязательная переменная '{name}' не задана в .env или окружении"
        ))
    })
}

/// Reads a numeric env variable of type u64.
/// Returns `default` if the variable is not set.
/// Returns `SentinelError::Config` if the variable is set but cannot be parsed as a number.
fn parse_u64_var(name: &str, default: u64) -> Result<u64, SentinelError> {
    match env::var(name) {
        // Variable not set — use default, no error
        Err(_) => Ok(default),
        // Variable set — try to parse it as u64
        Ok(val) => val.parse::<u64>().map_err(|_| {
            SentinelError::Config(format!(
                "переменная '{name}' должна быть числом, получено: '{val}'"
            ))
        }),
    }
}
