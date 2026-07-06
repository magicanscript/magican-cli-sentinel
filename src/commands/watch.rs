/// The `watch` command — continuous Solana node monitoring daemon.
///
/// Algorithm:
/// 1. Initialise LLM and Telegram clients once at startup
/// 2. Execute the first probe immediately (tick)
/// 3. Wait for `poll_interval` via `tokio::select!`, which is interrupted by Ctrl+C
/// 4. On each tick: probe → analyse → if alert needed and cooldown elapsed → LLM → Telegram
///
/// Cooldown prevents spam: if an alert was sent less than `alert_cooldown` ago,
/// the new alert is suppressed with a warning log.
///
/// On probe or send errors — logs and continues the loop (the daemon does not crash).
use std::time::Instant;

use anyhow::Result;
use tracing::{error, info, warn};

use crate::analysis;
use crate::config::Config;
use crate::llm::LlmClient;
use crate::metrics;
use crate::notify::TelegramClient;

/// Runs the infinite monitoring loop.
///
/// Terminates only on Ctrl+C (SIGINT).
/// All errors inside the loop are handled locally — the daemon does not panic.
pub async fn run(cfg: Config) -> Result<()> {
    info!("daemon started: {}", cfg.summary());

    let llm = LlmClient::new(&cfg)?;
    let tg = TelegramClient::new();
    let mut last_alert_at: Option<Instant> = None;
    let poll_interval = cfg.poll_interval;

    loop {
        tick(&cfg, &llm, &tg, &mut last_alert_at).await;

        // Wait for the next tick or Ctrl+C.
        // tokio::select! ensures the signal is handled even during sleep.
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                match result {
                    Ok(()) => info!("received Ctrl+C, shutting down..."),
                    Err(e) => error!("signal handler error: {e}"),
                }
                break;
            }
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }

    info!("daemon stopped");
    Ok(())
}

/// One probe cycle: probe → analyse → (optionally) alert.
///
/// Errors are logged and do not interrupt the daemon.
async fn tick(
    cfg: &Config,
    llm: &LlmClient,
    tg: &TelegramClient,
    last_alert_at: &mut Option<Instant>,
) {
    // Probe both nodes in parallel
    let probe = match metrics::probe_both(cfg).await {
        Ok(p) => p,
        Err(e) => {
            error!("node probe failed: {e}");
            return;
        }
    };

    let analysis = analysis::analyze(&probe, cfg);

    info!(
        "tick: delta={:+} target_rtt={}ms status={}",
        analysis.slot_delta,
        analysis.target_rtt_ms,
        analysis.status_text(),
    );

    if !analysis.needs_alert {
        return;
    }

    // Check cooldown: do not send alerts too frequently
    if let Some(last) = *last_alert_at {
        let elapsed = last.elapsed();
        if elapsed < cfg.alert_cooldown {
            let remaining = cfg.alert_cooldown.saturating_sub(elapsed);
            warn!(
                "alert suppressed by cooldown: {}s until next",
                remaining.as_secs()
            );
            return;
        }
    }

    // Generate alert text via LLM; fall back to a basic message on error
    let alert_text = match llm
        .generate_alert_text(&cfg.target_rpc_url, &analysis, cfg)
        .await
    {
        Ok(text) => text,
        Err(e) => {
            error!("LLM error: {e} — using fallback alert text");
            format!("ALERT {}: {}", cfg.target_rpc_url, analysis.status_text())
        }
    };

    // Send to Telegram
    if let Err(e) = tg.send_message(&alert_text, cfg).await {
        error!("failed to send Telegram alert: {e}");
        return;
    }

    *last_alert_at = Some(Instant::now());
    info!("alert sent: {alert_text}");
}
