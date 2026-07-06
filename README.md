# magican-cli-sentinel

![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)
![Solana](https://img.shields.io/badge/Solana-mainnet--beta-9945FF?logo=solana)
![LLM](https://img.shields.io/badge/LLM-OpenAI--compatible-blue)


## Features

- **Parallel node probing** — queries target and reference nodes concurrently via `tokio::try_join!`; total wall-clock ≈ max(rtt_target, rtt_reference)
- **Two alert conditions** — slot lag behind reference exceeds threshold, or target RTT exceeds threshold
- **AI-generated alerts** — sends raw metrics to an OpenAI-compatible Chat Completions API (Mistral by default; switchable via `LLM_BASE_URL`); gets a concise ≤ 200-char technical alert back
- **Telegram delivery** — posts alerts to any chat or channel via Bot API with HTML parse mode
- **Cooldown** — configurable minimum interval between alerts to prevent spam during prolonged incidents
- **Exponential backoff retry** — RPC calls, the LLM API, and Telegram API each retry on transient failures (RPC/Telegram 3×; LLM 4 attempts, 1s → 2s → 4s)
- **Graceful shutdown** — `Ctrl+C` interrupts the sleep phase and exits cleanly
- **Environment-based config** — all parameters via env vars; compatible with systemd, Docker, and `.env` files
- **Structured logging** — `tracing` with `RUST_LOG`-controlled verbosity

---

## Architecture

```
src/
├── main.rs               Entry point: load .env, init tracing, dispatch command
├── error.rs              SentinelError — typed errors for all subsystems
├── config/mod.rs         Config::from_env() — reads and validates all env vars
├── analysis/mod.rs       analyze() — pure function; computes slot delta and flags
├── metrics/mod.rs        probe_node(), probe_both() — RPC polling with retry
├── utils/mod.rs          retry_async() — generic exponential backoff helper
├── llm/mod.rs            LlmClient — OpenAI-compatible Chat Completions (rust-llm-client)
├── notify/mod.rs         TelegramClient — Telegram Bot API
└── commands/
    ├── mod.rs            Commands enum + dispatcher
    ├── status.rs         `status` — one-shot probe, prints result, exits 0/1
    └── watch.rs          `watch` — infinite polling loop with Ctrl+C shutdown
```

Data flow for `watch`:

```
probe_both()  →  analyze()  →  [needs_alert?]  →  LlmClient::generate_alert_text()
                                                →  TelegramClient::send_message()
```

---

## Prerequisites

- Rust 1.85+ (`cargo`)
- A Solana RPC endpoint to monitor (e.g. your own validator)
- An API key for an OpenAI-compatible LLM provider — e.g. [Mistral](https://console.mistral.ai/api-keys/) (default), OpenAI, or Groq; not needed for a local Ollama
- Telegram bot token (create via [@BotFather](https://t.me/BotFather)) and a chat/channel ID

---

## Configuration

All settings are read from environment variables. Copy `.env.example` to `.env` and fill in your values:

```bash
cp .env.example .env
```

| Variable | Required | Default | Description |
|---|---|---|---|
| `SOLANA_TARGET_RPC_URL` | ✅ | — | URL of the node to monitor (e.g. `http://192.168.1.10:8899`) |
| `SOLANA_REFERENCE_RPC_URL` | | `https://api.mainnet-beta.solana.com` | Reference node for slot comparison |
| `SENTINEL_POLL_INTERVAL_SECS` | | `10` | Seconds between probes |
| `SENTINEL_SLOT_LAG_THRESHOLD` | | `5` | Max allowed slot lag before alert |
| `SENTINEL_RTT_THRESHOLD_MS` | | `500` | Max allowed target RTT (ms) before alert |
| `SENTINEL_ALERT_COOLDOWN_SECS` | | `300` | Min seconds between two alerts |
| `LLM_API_KEY` | | — | API key for the LLM provider; empty/unset sends no `Authorization` header (for a keyless local Ollama) |
| `LLM_MODEL` | | `mistral-small-latest` | Model for alert generation |
| `LLM_BASE_URL` | | `https://api.mistral.ai/v1` | OpenAI-compatible API root; switches provider (OpenAI, Groq, Ollama, …) |
| `TELEGRAM_BOT_TOKEN` | ✅ | — | Telegram bot token from BotFather |
| `TELEGRAM_CHAT_ID` | ✅ | — | Chat or channel ID for alert delivery |

---

## Usage

### `status` — one-shot probe

Queries both nodes once, prints a report, and exits with code `0` (OK) or `1` (problem detected). Useful in shell scripts and cron jobs.

```bash
cargo run -- status
```


### `watch` — continuous daemon

Polls nodes on the configured interval, sends Telegram alerts when thresholds are breached, and respects the cooldown between alerts. Shuts down cleanly on `Ctrl+C`.

```bash
cargo run -- watch
```
**Debug logging:**

```bash
RUST_LOG=debug cargo run -- watch
```

---

## How It Works

1. **Probe** — `probe_both()` calls `getSlot` on both target and reference nodes in parallel, measuring RTT for each. Each call retries up to 3 times on failure.
2. **Analyse** — `analyze()` computes `slot_delta = target_slot - reference_slot` and checks both thresholds. Pure function, no I/O.
3. **Cooldown check** — if an alert was sent recently (within `alert_cooldown`), the new alert is suppressed with a warning log.
4. **LLM** — raw metrics are sent to the configured OpenAI-compatible provider (`LLM_BASE_URL`) with a prompt requesting a ≤ 200-char plain-text alert. Falls back to a template string if the API call fails.
5. **Telegram** — the generated text is posted to the configured chat via `sendMessage`. Retries on transient HTTP errors.

---

## Development

```bash
# Check compilation without linking (fast)
cargo check

# Build
cargo build

# Run unit tests (12 tests, no network required)
cargo test

# Run live integration tests (require real credentials in .env)
cargo test llm_live -- --ignored --nocapture
cargo test telegram_live -- --ignored --nocapture

# Run with debug logging
RUST_LOG=debug cargo run -- status
```
