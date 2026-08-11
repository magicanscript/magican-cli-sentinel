/// General-purpose utilities.
use std::future::Future;
use std::time::Duration;

use tracing::warn;

/// Retries an async operation on failure with exponential backoff.
///
/// Strategy: up to `max_attempts` attempts; delays between them: 1s, 2s, 4s, …
/// On exhaustion returns the last error without panicking.
///
/// # Arguments
/// * `label`        — operation label used in warning log lines
/// * `max_attempts` — total number of attempts (including the first)
/// * `op`           — future factory: called anew on each attempt
///
/// # Example
/// ```ignore
/// let slot = retry_async("get_slot", 3, || rpc.get_slot()).await?;
/// ```
pub async fn retry_async<F, Fut, T, E>(label: &str, max_attempts: u32, mut op: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=max_attempts {
        match op().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                if attempt < max_attempts {
                    warn!(
                        "{label}: attempt {attempt}/{max_attempts} failed ({e}), retrying in {}s",
                        delay.as_secs()
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                } else {
                    return Err(e);
                }
            }
        }
    }
    unreachable!()
}
