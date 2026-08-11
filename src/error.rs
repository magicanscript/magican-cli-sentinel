/// Central error type for the entire application.
///
/// Uses `thiserror` which auto-generates `Display` and `std::error::Error`
/// implementations via the `#[derive(Error)]` attribute.
///
/// Each variant maps to one error source:
/// - `Config`   — errors reading configuration from env variables
/// - `Rpc`      — errors talking to the Solana RPC (network, timeout, bad response)
/// - `Http`     — HTTP-level errors (reqwest) for the Telegram API
/// - `LlmClient`— error surfaced by the `rust-llm-client` crate (LLM calls)
/// - `Telegram` — Telegram Bot API returned `ok: false`
/// - `MalformedResponse` — a peer's response was oversized or not valid JSON
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SentinelError {
    /// A required env variable is missing or has an invalid format.
    /// `{0}` contains a human-readable description of what is wrong.
    #[error("Configuration error: {0}")]
    Config(String),

    /// Error querying a Solana RPC node.
    /// `{0}` contains the client message (address, error code, etc.).
    #[error("RPC error: {0}")]
    Rpc(String),

    /// HTTP-level error: network failures, timeouts, TLS issues.
    /// `#[from]` lets Rust auto-convert `reqwest::Error` into `SentinelError::Http` via `?`.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Error from the `rust-llm-client` crate: configuration, transport,
    /// rate-limit, API status, malformed response, or JSON-parse failure.
    /// `#[from]` auto-converts `rust_llm_client::LlmError` via `?`.
    #[error("LLM client error: {0}")]
    LlmClient(#[from] rust_llm_client::LlmError),

    /// Telegram API returned `ok: false` with an error description.
    #[error("Telegram error: {0}")]
    Telegram(String),

    /// A remote peer's response could not be accepted: it exceeded the
    /// response-size cap, or its body was not valid JSON.
    /// `{0}` describes which of the two occurred.
    #[error("Malformed response: {0}")]
    MalformedResponse(String),
}
