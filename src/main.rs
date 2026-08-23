/// Application entry point.
///
/// Initialization order:
/// 1. Load `.env` via `dotenvy` (silently ignored if the file is absent)
/// 2. Initialize logging via `tracing_subscriber`
///    — level is read from `RUST_LOG` (e.g. RUST_LOG=debug)
/// 3. Parse CLI arguments via `clap`
/// 4. Read configuration from environment variables
/// 5. Dispatch to the requested command
use anyhow::Result;
use clap::Parser;
use dotenvy::dotenv;

mod analysis;
mod commands;
mod config;
mod error;
mod llm;
mod metrics;
mod net;
mod notify;
mod utils;

#[derive(Parser)]
#[command(name = "solana-cli-sentinel")]
#[command(about = "Solana node monitoring daemon with AI-powered Telegram alerts")]
struct Cli {
    #[command(subcommand)]
    command: commands::Commands,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env if it exists; ok() silences the error when the file is missing
    dotenv().ok();

    // Initialize tracing.
    // Log level is read from RUST_LOG; defaults to info.
    // with_target(false) — strips the module path from each log line
    // Example: RUST_LOG=debug cargo run -- status
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    // Read configuration once and pass it into the command.
    // If any required env variable is missing the process exits with an error.
    let cfg = config::Config::from_env()?;

    commands::execute(cli.command, cfg).await?;

    Ok(())
}
