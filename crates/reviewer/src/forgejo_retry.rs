use std::future::Future;
use std::time::Duration;

use tracing::warn;

use chuggernaut_forgejo_api::ForgejoError;

const MAX_ATTEMPTS: u32 = 3;
const BASE_DELAY_MS: u64 = 2000;

/// Retry a Forgejo API call with exponential backoff (2s, 4s, 8s).
/// Returns the result of the last attempt on exhaustion.
pub async fn retry<F, Fut, T>(label: &str, f: F) -> Result<T, ForgejoError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, ForgejoError>>,
{
    let mut last_err = None;

    for attempt in 0..MAX_ATTEMPTS {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                let delay = Duration::from_millis(BASE_DELAY_MS * (1 << attempt));
                if attempt + 1 < MAX_ATTEMPTS {
                    warn!(
                        label,
                        attempt = attempt + 1,
                        max = MAX_ATTEMPTS,
                        delay_ms = delay.as_millis() as u64,
                        error = %e,
                        "Forgejo API call failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn succeeds_on_first_try() {
        let result = retry("test", || async { Ok::<_, ForgejoError>(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn retries_then_succeeds() {
        let attempts = AtomicU32::new(0);
        let result = retry("test", || {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(ForgejoError::Api {
                        status: 500,
                        body: "server error".to_string(),
                    })
                } else {
                    Ok(99)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 99);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn exhausts_retries() {
        let attempts = AtomicU32::new(0);
        let result: Result<(), _> = retry("test", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Err(ForgejoError::Api {
                    status: 503,
                    body: "unavailable".to_string(),
                })
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }
}
