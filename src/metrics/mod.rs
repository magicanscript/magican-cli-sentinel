/// Metrics collection module: probing Solana nodes and measuring RTT.
///
/// Main entry points, both on `RpcProbe`:
/// - `probe_node(url)` — probes a single node: fetches the current slot and measures RTT
/// - `probe_both(cfg)` — probes target and reference in parallel via `tokio::try_join!`
///
/// RTT is measured as the time from sending the `getSlot` RPC request to receiving
/// the response. This is not pure network RTT — it includes JSON serialisation and
/// server-side processing — but it is a reliable indicator of node availability and
/// responsiveness.
///
/// The `getSlot` call is issued as a plain JSON-RPC POST rather than through
/// `solana-rpc-client`, because that crate buffers the response body with no upper
/// bound: a hostile or MITM'd node could answer one probe with an arbitrarily large
/// body and exhaust the daemon's memory. Going through `crate::net` applies the same
/// timeout and byte cap here as on the Telegram path.
use std::time::Instant;

use reqwest::Client;
use serde_json::json;
use tracing::debug;

use crate::config::Config;
use crate::error::SentinelError;
use crate::net;
use crate::utils;

/// Commitment level requested from `getSlot`.
///
/// This is what `solana-rpc-client`'s `RpcClient::new()` asked for by default
/// (`CommitmentConfig::default()` is `finalized`), and it is named explicitly so
/// the lag figures keep meaning what they meant before the transport changed.
const COMMITMENT: &str = "finalized";

/// Upper bound on an RPC response body. A `getSlot` reply is a few dozen bytes;
/// this leaves several orders of magnitude of headroom before the guard trips.
const MAX_RPC_RESPONSE_BYTES: usize = net::MAX_RESPONSE_BYTES;

/// Metrics for a single probe of a single node.
#[derive(Debug, Clone)]
pub struct NodeMetrics {
    /// Current slot of the node at the time of the probe.
    pub slot: u64,

    /// Node response time in milliseconds (RTT).
    pub rtt_ms: u64,

    /// Node URL (used for logging and alerts).
    pub node_url: String,
}

/// Result of a parallel probe of both nodes in one cycle.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// Metrics for the monitored (target) node.
    pub target: NodeMetrics,

    /// Metrics for the reference node.
    pub reference: NodeMetrics,
}

/// Issues `getSlot` probes against Solana nodes over a bounded HTTP client.
///
/// Holds one `reqwest::Client` so the connection pool is reused across ticks of
/// the daemon's polling loop.
pub struct RpcProbe {
    http: Client,
}

impl RpcProbe {
    /// Builds a prober. Call once at application startup.
    ///
    /// # Errors
    /// - `SentinelError::Http` — the underlying TLS/resolver backend failed to
    ///   initialise, which is a startup fault rather than a per-request one.
    pub fn new() -> Result<Self, SentinelError> {
        let http = net::build_client(net::DEFAULT_REQUEST_TIMEOUT, net::DEFAULT_CONNECT_TIMEOUT)?;

        Ok(Self { http })
    }

    /// Probes a single Solana node: fetches the current slot and measures response time.
    ///
    /// # Errors
    /// - `SentinelError::Http` — the node is unreachable or exceeded the request timeout
    /// - `SentinelError::MalformedResponse` — the body was oversized or not valid JSON
    /// - `SentinelError::Rpc` — the node answered with a JSON-RPC error, or with a
    ///   `result` that is not a slot number
    pub async fn probe_node(&self, url: &str) -> Result<NodeMetrics, SentinelError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSlot",
            "params": [{ "commitment": COMMITMENT }],
        });

        // Start the timer immediately before the request
        let start = Instant::now();
        let response = self
            .http
            .post(url)
            .json(&request)
            .send()
            .await
            .map_err(SentinelError::Http)?;
        let body = net::read_json_capped(response, MAX_RPC_RESPONSE_BYTES).await?;
        let rtt_ms = start.elapsed().as_millis() as u64;

        let slot = parse_slot(&body, url)?;

        debug!(url, slot, rtt_ms, "node probed");

        Ok(NodeMetrics {
            slot,
            rtt_ms,
            node_url: url.to_string(),
        })
    }

    /// Probes both nodes from the configuration in parallel with automatic retry.
    ///
    /// Each node is probed independently with exponential backoff (up to 3 attempts).
    /// `tokio::try_join!` runs both retry loops concurrently — total wall-clock time
    /// ≈ max(rtt_target, rtt_reference), not their sum.
    pub async fn probe_both(&self, cfg: &Config) -> Result<ProbeResult, SentinelError> {
        let target_url = cfg.target_rpc_url.clone();
        let reference_url = cfg.reference_rpc_url.clone();

        let (target, reference) = tokio::try_join!(
            utils::retry_async("target rpc", 3, || self.probe_node(&target_url)),
            utils::retry_async("reference rpc", 3, || self.probe_node(&reference_url)),
        )?;

        Ok(ProbeResult { target, reference })
    }
}

/// Extracts the slot number from a `getSlot` JSON-RPC reply.
///
/// The node is untrusted, so every shape other than a JSON-RPC success carrying an
/// unsigned integer `result` is an error rather than a value to coerce: a negative
/// or fractional `result`, or one past `u64::MAX`, is a broken or hostile node, not
/// a slot. `url` is included in the message so the operator can tell which of the
/// two nodes misbehaved.
fn parse_slot(body: &serde_json::Value, url: &str) -> Result<u64, SentinelError> {
    if let Some(error) = body.get("error") {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        return Err(SentinelError::Rpc(format!(
            "{url}: getSlot failed: {message}"
        )));
    }

    body.get("result")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| SentinelError::Rpc(format!("{url}: getSlot returned no usable slot number")))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::tests::response_with;

    const URL: &str = "http://node.example:8899";

    #[test]
    fn test_parse_slot_reads_a_normal_reply() {
        let body = json!({ "jsonrpc": "2.0", "result": 300_123_456u64, "id": 1 });

        assert_eq!(
            parse_slot(&body, URL).expect("a well-formed reply must parse"),
            300_123_456
        );
    }

    #[test]
    fn test_parse_slot_accepts_the_extremes_of_the_slot_range() {
        // Both bounds are legal u64 values; the analysis layer, not this one,
        // decides what an implausible slot number means.
        assert_eq!(parse_slot(&json!({ "result": 0 }), URL).unwrap_or(1), 0);
        assert_eq!(
            parse_slot(&json!({ "result": u64::MAX }), URL).unwrap_or(0),
            u64::MAX
        );
    }

    #[test]
    fn test_parse_slot_surfaces_a_json_rpc_error() {
        let body = json!({
            "jsonrpc": "2.0",
            "error": { "code": -32601, "message": "Method not found" },
            "id": 1,
        });

        match parse_slot(&body, URL) {
            Err(SentinelError::Rpc(msg)) => {
                assert!(msg.contains("Method not found"));
                assert!(msg.contains(URL));
            }
            other => panic!("expected an Rpc error, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_slot_rejects_an_error_without_a_message() {
        // A malformed error object must still be reported, not silently ignored.
        let body = json!({ "error": {} });

        assert!(matches!(parse_slot(&body, URL), Err(SentinelError::Rpc(_))));
    }

    #[test]
    fn test_parse_slot_rejects_results_that_are_not_slot_numbers() {
        // A hostile node cannot smuggle a non-slot through as one.
        for result in [
            json!({ "result": -1 }),
            json!({ "result": 1.5 }),
            json!({ "result": "300123456" }),
            json!({ "result": null }),
            json!({ "result": [1, 2, 3] }),
            json!({ "jsonrpc": "2.0", "id": 1 }),
        ] {
            assert!(
                matches!(parse_slot(&result, URL), Err(SentinelError::Rpc(_))),
                "must reject {result}"
            );
        }
    }

    #[tokio::test]
    async fn test_rpc_reply_is_read_under_the_cap() {
        // The whole probe path reads through `net::read_json_capped`; an
        // oversized reply from a node is refused the same way Telegram's is.
        let body = vec![b'x'; MAX_RPC_RESPONSE_BYTES + 1];
        let err = net::read_json_capped(response_with(body, true), MAX_RPC_RESPONSE_BYTES)
            .await
            .expect_err("an oversized RPC body must be refused");

        assert!(matches!(err, SentinelError::MalformedResponse(_)));
    }

    #[tokio::test]
    async fn test_a_normal_rpc_reply_survives_the_capped_read() {
        let body = br#"{"jsonrpc":"2.0","result":300123456,"id":1}"#.to_vec();
        let value = net::read_json_capped(response_with(body, true), MAX_RPC_RESPONSE_BYTES)
            .await
            .expect("a well-formed reply must parse");

        assert_eq!(parse_slot(&value, URL).unwrap_or(0), 300_123_456);
    }

    #[test]
    fn test_prober_is_built_with_a_bounded_client() {
        assert!(RpcProbe::new().is_ok());
    }
}
