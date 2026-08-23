/// Bounded HTTP primitives shared by every outbound client.
///
/// All three network peers this daemon talks to — the Solana RPC nodes, the LLM
/// provider, and the Telegram Bot API — are untrusted: a hostile or MITM'd
/// endpoint can answer with an arbitrarily large body, or accept the connection
/// and then never finish sending. Either behaviour stalls or exhausts a daemon
/// that reads with `reqwest`'s defaults, which impose no timeout and no cap on
/// body size.
///
/// This module holds the two guards that make such a peer harmless, so each
/// client applies the same bound rather than reinventing it:
/// - `build_client` — a `reqwest::Client` with request and connect deadlines
/// - `read_json_capped` — a body read that refuses to buffer past a byte budget
use std::time::Duration;

use reqwest::{Client, Response};

use crate::error::SentinelError;

/// Deadline for a single request, covering connect through body read.
/// Without it a peer that accepts the connection and then stops sending would
/// block the daemon's polling loop forever.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Deadline for establishing the TCP/TLS connection alone.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on a response body, applied to every peer.
///
/// The three replies this daemon reads — `getSlot`, a short chat completion, and
/// a Telegram `sendMessage` ack — are all well under a kilobyte in practice.
/// Anything approaching this cap is a peer trying to exhaust memory, not a
/// legitimate answer.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Builds an HTTP client whose requests cannot hang indefinitely.
///
/// # Errors
/// - `SentinelError::Http` — the underlying TLS/resolver backend failed to
///   initialise, which is a startup fault rather than a per-request one.
pub fn build_client(
    request_timeout: Duration,
    connect_timeout: Duration,
) -> Result<Client, SentinelError> {
    Client::builder()
        .timeout(request_timeout)
        .connect_timeout(connect_timeout)
        .build()
        .map_err(SentinelError::Http)
}

/// Reads a response body into JSON, refusing to buffer more than `max_bytes`.
///
/// `Response::json()` buffers the whole body with no upper bound, so a hostile
/// peer can drive the daemon out of memory with one oversized reply. This reads
/// the body chunk by chunk against a fixed budget instead, so the allocation
/// stays bounded no matter what the peer sends.
///
/// A declared `Content-Length` over the cap is rejected before any body is read;
/// a peer that lies about (or omits) it is caught by the per-chunk budget.
///
/// # Errors
/// - `SentinelError::MalformedResponse` — the body exceeded `max_bytes` or was
///   not valid JSON
/// - `SentinelError::Http` — the transport failed mid-body
pub async fn read_json_capped(
    mut response: Response,
    max_bytes: usize,
) -> Result<serde_json::Value, SentinelError> {
    if let Some(declared) = response.content_length()
        && declared > max_bytes as u64
    {
        return Err(SentinelError::MalformedResponse(format!(
            "response declares {declared} bytes, over the {max_bytes}-byte cap"
        )));
    }

    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(SentinelError::Http)? {
        check_budget(buf.len(), chunk.len(), max_bytes)?;
        buf.extend_from_slice(&chunk);
    }

    serde_json::from_slice(&buf)
        .map_err(|e| SentinelError::MalformedResponse(format!("body is not valid JSON: {e}")))
}

/// Rejects the next chunk if accepting it would push the buffered body past
/// `max_bytes`.
///
/// This is the guard that catches a peer which understates or omits its
/// `Content-Length`; the declared-length check alone trusts the peer's own
/// claim. `saturating_add` keeps the bound itself free of overflow.
fn check_budget(buffered: usize, incoming: usize, max_bytes: usize) -> Result<(), SentinelError> {
    if buffered.saturating_add(incoming) > max_bytes {
        return Err(SentinelError::MalformedResponse(format!(
            "response body exceeds the {max_bytes}-byte cap"
        )));
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds an in-memory `Response` with the given body — no network, no
    /// server. `declare_len` controls whether a truthful `Content-Length` is
    /// advertised, which lets the tests cover both the honest and the lying peer.
    ///
    /// Shared with the client modules so their tests can feed a hostile reply
    /// through the real code path.
    pub(crate) fn response_with(body: Vec<u8>, declare_len: bool) -> Response {
        let mut builder = http::Response::builder().status(200);
        if declare_len {
            builder = builder.header("content-length", body.len());
        }
        let built = builder
            .body(body)
            .unwrap_or_else(|_| http::Response::new(Vec::new()));
        Response::from(built)
    }

    #[tokio::test]
    async fn test_read_json_capped_accepts_a_normal_reply() {
        let body = br#"{"ok":true,"result":{"message_id":1}}"#.to_vec();
        let value = read_json_capped(response_with(body, true), MAX_RESPONSE_BYTES)
            .await
            .expect("a well-formed reply must parse");

        assert_eq!(value["ok"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn test_read_json_capped_rejects_declared_oversize_body() {
        // Honest peer advertising a body past the cap: rejected up front.
        let body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        let err = read_json_capped(response_with(body, true), MAX_RESPONSE_BYTES)
            .await
            .expect_err("an oversized body must be refused");

        match err {
            SentinelError::MalformedResponse(msg) => assert!(msg.contains("declares")),
            other => panic!("expected MalformedResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_read_json_capped_rejects_oversize_body_without_the_header() {
        // Omitting the Content-Length header must not get a body past the cap.
        // (An in-memory `Response` still knows its own length, so this lands on
        // the declared-length check; the streaming budget that catches a peer
        // which genuinely understates its length is covered by the
        // `check_budget` tests below.)
        let body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        let err = read_json_capped(response_with(body, false), MAX_RESPONSE_BYTES)
            .await
            .expect_err("an oversized body must be refused without Content-Length too");

        assert!(matches!(err, SentinelError::MalformedResponse(_)));
    }

    #[tokio::test]
    async fn test_read_json_capped_accepts_body_exactly_at_the_cap() {
        // The cap is inclusive: a body of exactly `max_bytes` is read.
        // Pad a valid JSON document out to the limit with insignificant spaces.
        let prefix = br#"{"ok":true}"#;
        let mut body = prefix.to_vec();
        body.resize(MAX_RESPONSE_BYTES, b' ');

        let value = read_json_capped(response_with(body, true), MAX_RESPONSE_BYTES)
            .await
            .expect("a body exactly at the cap must be accepted");

        assert_eq!(value["ok"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn test_read_json_capped_honours_a_smaller_cap() {
        // The cap is per-caller, not global: a body a large caller would accept
        // is still refused for one that asked for a tighter budget.
        let body = vec![b' '; 4096];
        let err = read_json_capped(response_with(body, true), 1024)
            .await
            .expect_err("a body over the caller's own cap must be refused");

        assert!(matches!(err, SentinelError::MalformedResponse(_)));
    }

    #[tokio::test]
    async fn test_read_json_capped_rejects_non_json_body() {
        let err = read_json_capped(
            response_with(b"<html>not json</html>".to_vec(), true),
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect_err("a non-JSON body must be refused");

        match err {
            SentinelError::MalformedResponse(msg) => assert!(msg.contains("not valid JSON")),
            other => panic!("expected MalformedResponse, got {other:?}"),
        }
    }

    #[test]
    fn test_check_budget_admits_chunks_up_to_the_cap() {
        assert!(check_budget(0, 0, MAX_RESPONSE_BYTES).is_ok());
        assert!(check_budget(0, MAX_RESPONSE_BYTES, MAX_RESPONSE_BYTES).is_ok());
        assert!(check_budget(MAX_RESPONSE_BYTES - 1, 1, MAX_RESPONSE_BYTES).is_ok());
    }

    #[test]
    fn test_check_budget_rejects_the_chunk_that_crosses_the_cap() {
        // A peer streaming past the limit is stopped at the first chunk that
        // would exceed it — the buffer never grows beyond the cap.
        assert!(check_budget(MAX_RESPONSE_BYTES, 1, MAX_RESPONSE_BYTES).is_err());
        assert!(check_budget(MAX_RESPONSE_BYTES - 1, 2, MAX_RESPONSE_BYTES).is_err());
        assert!(check_budget(0, MAX_RESPONSE_BYTES + 1, MAX_RESPONSE_BYTES).is_err());
    }

    #[test]
    fn test_check_budget_does_not_overflow_on_absurd_lengths() {
        // The bound itself must not wrap, even on nonsense inputs.
        assert!(check_budget(usize::MAX, usize::MAX, MAX_RESPONSE_BYTES).is_err());
    }

    #[test]
    fn test_build_client_applies_bounded_deadlines() {
        // The constructor must not hand back a client that can hang forever.
        assert!(build_client(DEFAULT_REQUEST_TIMEOUT, DEFAULT_CONNECT_TIMEOUT).is_ok());
        assert!(DEFAULT_REQUEST_TIMEOUT > Duration::ZERO);
        assert!(DEFAULT_CONNECT_TIMEOUT <= DEFAULT_REQUEST_TIMEOUT);
    }
}
