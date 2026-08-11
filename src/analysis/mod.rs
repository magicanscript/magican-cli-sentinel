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
    ///
    /// Saturates at `i64::MIN`/`i64::MAX` rather than wrapping when a node
    /// reports an implausible slot number. This field is for display only —
    /// the alert decision uses `is_slot_lagging`, which never saturates.
    pub slot_delta: i64,

    /// RTT of the request to the target node in milliseconds.
    pub target_rtt_ms: u64,

    /// RTT of the request to the reference node in milliseconds (for context).
    pub reference_rtt_ms: u64,

    /// `true` if the target node's slot lag exceeds `slot_lag_threshold`.
    /// Condition: `reference_slot - target_slot > config.slot_lag_threshold`,
    /// evaluated in `u64` space so no hostile slot number can flip it.
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
/// - `slot_delta` = target_slot - reference_slot (can be negative, saturating)
/// - The node is lagging if it is more than `slot_lag_threshold` slots behind the reference
/// - RTT is high if it exceeds `rtt_threshold_ms`
pub fn analyze(probe: &ProbeResult, cfg: &Config) -> Analysis {
    // Slot numbers arrive unvalidated from `getSlot` on two RPC endpoints, either of
    // which may be hostile or MITM'd. Both values below are therefore computed without
    // any arithmetic that could overflow, wrap, or panic on an adversarial `u64`.

    // Display value only: saturates instead of wrapping on implausible inputs.
    let slot_delta = signed_slot_delta(probe.target.slot, probe.reference.slot);

    // Alert decision: how far the target is *behind*, in u64 space. `saturating_sub`
    // yields 0 when the target is level with or ahead of the reference, so the
    // comparison stays correct for every possible pair of slot numbers.
    // Example: threshold=5, lag=7 → lagging. lag=3 → OK.
    let lag = probe.reference.slot.saturating_sub(probe.target.slot);
    let is_slot_lagging = lag > cfg.slot_lag_threshold;

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

/// Signed distance `target - reference`, clamped to the `i64` range.
///
/// The naive `target as i64 - reference as i64` wraps (release) or panics (debug)
/// when the two slot numbers are far enough apart, which a hostile RPC endpoint can
/// arrange with a single crafted response. This computes the gap in `u64` space —
/// where it always fits — and only then applies the sign, saturating if the
/// magnitude exceeds what an `i64` can hold.
fn signed_slot_delta(target: u64, reference: u64) -> i64 {
    if target >= reference {
        i64::try_from(target - reference).unwrap_or(i64::MAX)
    } else {
        // `try_from` yields at most `i64::MAX`, so negation never overflows.
        i64::try_from(reference - target).map_or(i64::MIN, |gap| -gap)
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

    // ------------------------------------------------------------------
    // Hostile / extreme slot numbers (ARITHOFL-001)
    //
    // Either RPC endpoint may return an arbitrary u64. None of the values
    // below may panic, wrap, or flip the lag decision to a false negative.
    // ------------------------------------------------------------------

    #[test]
    fn test_signed_slot_delta_saturates_instead_of_wrapping() {
        // Ordinary cases keep their exact value and sign.
        assert_eq!(signed_slot_delta(100, 100), 0);
        assert_eq!(signed_slot_delta(110, 100), 10);
        assert_eq!(signed_slot_delta(90, 100), -10);

        // Gaps too large for an i64 clamp to the bounds rather than wrapping.
        assert_eq!(signed_slot_delta(u64::MAX, 0), i64::MAX);
        assert_eq!(signed_slot_delta(0, u64::MAX), i64::MIN);
    }

    #[test]
    fn test_hostile_reference_slot_does_not_suppress_alert() {
        // A malicious reference node claims a wildly high slot. The old
        // `as i64` subtraction wrapped here and reported "not lagging";
        // the daemon must now see a massive lag and alert.
        let cfg = make_config(5, 500);
        let probe = make_probe(100, 200, u64::MAX);
        let analysis = analyze(&probe, &cfg);

        assert_eq!(analysis.slot_delta, i64::MIN);
        assert!(analysis.is_slot_lagging);
        assert!(analysis.needs_alert);
    }

    #[test]
    fn test_hostile_target_slot_does_not_trigger_false_alert() {
        // A malicious target node claims a wildly high slot: it is ahead,
        // not behind, so no slot-lag alert — and no panic on the way.
        let cfg = make_config(5, 500);
        let probe = make_probe(u64::MAX, 200, 100);
        let analysis = analyze(&probe, &cfg);

        assert_eq!(analysis.slot_delta, i64::MAX);
        assert!(!analysis.is_slot_lagging);
        assert!(!analysis.needs_alert);
    }

    #[test]
    fn test_lag_decision_survives_every_slot_extreme() {
        // Exhaustive over the interesting corners: no combination may panic.
        let cfg = make_config(5, 500);
        let extremes = [0, 1, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX];

        for target in extremes {
            for reference in extremes {
                let analysis = analyze(&make_probe(target, 200, reference), &cfg);
                // The decision must agree with the exact unsigned lag.
                let expected = reference.saturating_sub(target) > cfg.slot_lag_threshold;
                assert_eq!(
                    analysis.is_slot_lagging, expected,
                    "target={target} reference={reference}"
                );
            }
        }
    }

    #[test]
    fn test_max_threshold_never_reports_lagging() {
        // An operator-set threshold of u64::MAX cannot be exceeded by any lag.
        let cfg = make_config(u64::MAX, 500);
        let probe = make_probe(0, 200, u64::MAX);
        let analysis = analyze(&probe, &cfg);

        assert!(!analysis.is_slot_lagging);
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
