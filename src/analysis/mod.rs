/// Metrics analysis module.
///
/// Contains the single public function `analyze()`, which takes the result of
/// probing two nodes plus the configuration, and returns a structured analysis
/// with violation flags.
///
/// This module intentionally makes no network requests — only arithmetic and
/// comparisons. This makes it easy to unit-test without a real Solana node.
use crate::config::Config;
use crate::metrics::ProbeResult;

/// Result of analysing one probe cycle.
///
/// Contains both raw computed values (for logging) and boolean flags
/// (for deciding whether to send an alert).
#[derive(Debug, Clone)]
pub struct Analysis {
    /// Slot difference: `target_slot - reference_slot`.
    /// A negative value means the target node is behind the reference.
    /// For example, -12 means our node is 12 slots behind the reference.
    pub slot_delta: i64,

    /// RTT of the request to the target node in milliseconds.
    pub target_rtt_ms: u64,

    /// RTT of the request to the reference node in milliseconds (for context).
    pub reference_rtt_ms: u64,

    /// `true` if the target node's slot lag exceeds `slot_lag_threshold`.
    /// Condition: `slot_delta < -(config.slot_lag_threshold as i64)`
    pub is_slot_lagging: bool,

    /// `true` if the target node's RTT exceeds `rtt_threshold_ms`.
    pub is_rtt_high: bool,

    /// `true` if an alert should be sent (at least one condition is violated).
    /// `needs_alert = is_slot_lagging || is_rtt_high`
    pub needs_alert: bool,
}

impl Analysis {
    /// Returns a human-readable description of the problem for logging.
    /// Returns "OK" if no problems are detected.
    pub fn status_text(&self) -> String {
        if !self.needs_alert {
            return "OK".to_string();
        }
        let mut parts = Vec::new();
        if self.is_slot_lagging {
            parts.push(format!(
                "отставание слотов: {} (порог: нарушен)",
                self.slot_delta
            ));
        }
        if self.is_rtt_high {
            parts.push(format!(
                "высокий RTT: {}ms (ref: {}ms)",
                self.target_rtt_ms, self.reference_rtt_ms
            ));
        }
        parts.join(", ")
    }
}

/// Analyses the result of a node probe and returns an `Analysis` struct.
///
/// # Arguments
/// * `probe` — result of the parallel probe of target and reference nodes
/// * `cfg`   — configuration with thresholds for comparison
///
/// # Logic
/// - `slot_delta` = target_slot - reference_slot (can be negative)
/// - The node is lagging if it is more than `slot_lag_threshold` slots behind the reference
/// - RTT is high if it exceeds `rtt_threshold_ms`
pub fn analyze(probe: &ProbeResult, cfg: &Config) -> Analysis {
    // Compute slot delta.
    // Cast u64 to i64 so the delta can be negative.
    let slot_delta = probe.target.slot as i64 - probe.reference.slot as i64;

    // The node is lagging if the delta is negative AND its absolute value exceeds the threshold.
    // Example: threshold=5, delta=-7 → lagging. delta=-3 → OK.
    let is_slot_lagging = slot_delta < -(cfg.slot_lag_threshold as i64);

    // RTT is high if it exceeds the configured threshold in milliseconds.
    let is_rtt_high = probe.target.rtt_ms > cfg.rtt_threshold_ms;

    Analysis {
        slot_delta,
        target_rtt_ms: probe.target.rtt_ms,
        reference_rtt_ms: probe.reference.rtt_ms,
        is_slot_lagging,
        is_rtt_high,
        needs_alert: is_slot_lagging || is_rtt_high,
    }
}

// ============================================================================
// Unit tests
// ============================================================================
//
// Run all: cargo test
// Run one: cargo test analysis::tests::test_no_alert

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::NodeMetrics;
    use std::time::Duration;

    /// Creates a test Config with the given thresholds.
    /// API key fields are stubs — not used in these tests.
    fn make_config(slot_lag_threshold: u64, rtt_threshold_ms: u64) -> Config {
        Config {
            target_rpc_url: "http://localhost:8899".to_string(),
            reference_rpc_url: "https://api.mainnet-beta.solana.com".to_string(),
            poll_interval: Duration::from_secs(10),
            slot_lag_threshold,
            rtt_threshold_ms,
            alert_cooldown: Duration::from_secs(300),
            llm_api_key: "test-key".to_string(),
            llm_model: "mistral-small-latest".to_string(),
            llm_base_url: "https://api.mistral.ai/v1".to_string(),
            telegram_bot_token: "test-token".to_string(),
            telegram_chat_id: "test-chat".to_string(),
        }
    }

    /// Creates a test ProbeResult with the given slot and RTT values.
    fn make_probe(target_slot: u64, target_rtt_ms: u64, reference_slot: u64) -> ProbeResult {
        ProbeResult {
            target: NodeMetrics {
                slot: target_slot,
                rtt_ms: target_rtt_ms,
                node_url: "http://localhost:8899".to_string(),
            },
            reference: NodeMetrics {
                slot: reference_slot,
                rtt_ms: 50, // reference RTT does not affect alert logic
                node_url: "https://api.mainnet-beta.solana.com".to_string(),
            },
        }
    }

    #[test]
    fn test_no_alert_when_everything_is_fine() {
        // Node is not lagging (delta = -3, threshold = 5), RTT is OK (200ms < 500ms)
        let cfg = make_config(5, 500);
        let probe = make_probe(100_000 - 3, 200, 100_000);
        let analysis = analyze(&probe, &cfg);

        assert_eq!(analysis.slot_delta, -3);
        assert!(!analysis.is_slot_lagging);
        assert!(!analysis.is_rtt_high);
        assert!(!analysis.needs_alert);
    }

    #[test]
    fn test_alert_when_slot_lagging() {
        // Node is 10 slots behind with a threshold of 5 → alert
        let cfg = make_config(5, 500);
        let probe = make_probe(100_000 - 10, 200, 100_000);
        let analysis = analyze(&probe, &cfg);

        assert_eq!(analysis.slot_delta, -10);
        assert!(analysis.is_slot_lagging);
        assert!(!analysis.is_rtt_high);
        assert!(analysis.needs_alert);
    }

    #[test]
    fn test_alert_when_rtt_high() {
        // RTT = 800ms with a threshold of 500ms → alert, slots are OK
        let cfg = make_config(5, 500);
        let probe = make_probe(100_000, 800, 100_000);
        let analysis = analyze(&probe, &cfg);

        assert!(!analysis.is_slot_lagging);
        assert!(analysis.is_rtt_high);
        assert!(analysis.needs_alert);
    }

    #[test]
    fn test_alert_when_both_conditions_violated() {
        // Both slot lag AND high RTT at the same time
        let cfg = make_config(5, 500);
        let probe = make_probe(100_000 - 20, 1200, 100_000);
        let analysis = analyze(&probe, &cfg);

        assert!(analysis.is_slot_lagging);
        assert!(analysis.is_rtt_high);
        assert!(analysis.needs_alert);
    }

    #[test]
    fn test_no_alert_at_exact_threshold() {
        // delta = -5 with threshold 5: NOT lagging (strict inequality: < -5)
        let cfg = make_config(5, 500);
        let probe = make_probe(100_000 - 5, 500, 100_000);
        let analysis = analyze(&probe, &cfg);

        // slot_delta = -5, threshold = 5: condition slot_delta < -5 → false
        assert!(!analysis.is_slot_lagging);
        // rtt = 500, threshold = 500: condition rtt > 500 → false
        assert!(!analysis.is_rtt_high);
        assert!(!analysis.needs_alert);
    }

    #[test]
    fn test_target_ahead_of_reference() {
        // target is ahead of reference (positive delta) — this is normal
        let cfg = make_config(5, 500);
        let probe = make_probe(100_010, 100, 100_000);
        let analysis = analyze(&probe, &cfg);

        assert_eq!(analysis.slot_delta, 10);
        assert!(!analysis.is_slot_lagging);
        assert!(!analysis.needs_alert);
    }

    #[test]
    fn test_status_text_ok() {
        let cfg = make_config(5, 500);
        let probe = make_probe(100_000, 100, 100_000);
        let analysis = analyze(&probe, &cfg);
        assert_eq!(analysis.status_text(), "OK");
    }

    #[test]
    fn test_status_text_lagging() {
        let cfg = make_config(5, 500);
        let probe = make_probe(99_990, 100, 100_000);
        let analysis = analyze(&probe, &cfg);
        assert!(analysis.status_text().contains("отставание слотов"));
    }
}
