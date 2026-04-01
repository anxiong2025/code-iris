//! Retry policy — exponential back-off for transient API errors.
//!
//! Retryable status codes (same as claw-code / Claude Code):
//! 408 Request Timeout, 429 Rate Limited, 500/502/503/504 Server Errors.
//!
//! Back-off: 200ms → 400ms → 800ms … capped at 8s, max 4 attempts.

use std::time::Duration;
use tokio::time::sleep;

/// Which HTTP status codes trigger a retry.
pub fn is_retryable(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// Configuration for the retry loop.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of attempts (first try + retries).
    pub max_attempts: u32,
    /// Initial back-off duration (doubles each attempt).
    pub initial_backoff: Duration,
    /// Maximum back-off duration.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    /// Back-off duration for the nth retry (0-indexed).
    pub fn backoff(&self, attempt: u32) -> Duration {
        let ms = self.initial_backoff.as_millis() * (1u128 << attempt);
        Duration::from_millis(ms.min(self.max_backoff.as_millis()) as u64)
    }

    /// Run `f` with retry on transient errors.
    ///
    /// `f` returns `Ok(T)` on success or `Err((status_code, message))` on failure.
    /// When the status code is retryable and attempts remain, the call is retried
    /// after an exponential back-off delay.
    pub async fn run<F, Fut, T>(&self, mut f: F) -> Result<T, String>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, (u16, String)>>,
    {
        for attempt in 0..self.max_attempts {
            match f().await {
                Ok(val) => return Ok(val),
                Err((status, msg)) => {
                    let is_last = attempt + 1 >= self.max_attempts;
                    if is_last || !is_retryable(status) {
                        return Err(format!("HTTP {status}: {msg}"));
                    }
                    let delay = self.backoff(attempt);
                    tracing::warn!(
                        attempt = attempt + 1,
                        status,
                        delay_ms = delay.as_millis(),
                        "retrying after transient error"
                    );
                    sleep(delay).await;
                }
            }
        }
        unreachable!()
    }
}
