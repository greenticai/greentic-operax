//! Reconnect loop with capped exponential backoff for the daemon's NATS subscribers.
//!
//! All three subscribers (`presence::run_presence_subscriber`,
//! `business_events::run_subscriber`, `business_events::run_event_router`) spin their
//! own current-thread tokio runtime and connect+subscribe+consume in a loop. Previously a
//! dropped NATS connection (broker restart, network blip) meant the subscriber thread
//! exited silently and never reconnected. [`run_with_reconnect`] wraps the
//! connect+subscribe+consume step so a failed or ended session is retried forever with
//! escalating backoff instead of terminating the thread.

use std::time::Duration;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Next backoff: double, capped at `MAX_BACKOFF`.
pub fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(MAX_BACKOFF)
}

/// Run `session` repeatedly forever: each call connects+subscribes+consumes until it
/// returns (stream ended) or errors; then log and sleep with escalating backoff before
/// retrying. `sleep` is injected so tests don't actually sleep. `label` names the
/// subscriber in logs. This never returns under normal operation (the daemon thread owns
/// it) — callers running it inside a `block_on` should not expect control to come back.
pub async fn run_with_reconnect<F, Fut, S, SFut>(label: &str, mut session: F, sleep: S)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
    S: Fn(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match session().await {
            Ok(()) => {
                eprintln!("[operax serve] {label} subscription ended; reconnecting in {backoff:?}")
            }
            Err(e) => eprintln!(
                "[operax serve] {label} subscriber error: {e}; reconnecting in {backoff:?}"
            ),
        }
        sleep(backoff).await;
        backoff = next_backoff(backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_backoff_doubles_and_caps() {
        let mut backoff = INITIAL_BACKOFF;
        let expected = [2, 4, 8, 16, 30, 30, 30];
        for expected_secs in expected {
            backoff = next_backoff(backoff);
            assert_eq!(backoff, Duration::from_secs(expected_secs));
        }
    }

    /// `run_with_reconnect` never returns, so this test bounds the run with a real (not
    /// paused) `tokio::time::timeout`. `session` and the injected `sleep` both resolve
    /// without ever awaiting real I/O; if they never yielded control back to the executor
    /// at all, the outer future's `poll()` would loop forever inside a single call and
    /// the timeout would never get a chance to run alongside it — so the injected `sleep`
    /// does one `tokio::task::yield_now().await` per call, just enough cooperative
    /// yielding for the timeout to interleave and fire on real wall-clock time. The
    /// assertion that matters is that the shared counter advanced multiple times within
    /// the timeout window, proving the combinator retries after both `Ok` and `Err`
    /// returns instead of stopping at the first one.
    #[tokio::test]
    async fn run_with_reconnect_retries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_session = calls.clone();

        let session = move || {
            let calls = calls_for_session.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n % 2 == 0 {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("simulated session error"))
                }
            }
        };
        let noop_sleep = |_d: Duration| async { tokio::task::yield_now().await };

        let _ = tokio::time::timeout(
            Duration::from_millis(50),
            run_with_reconnect("test", session, noop_sleep),
        )
        .await;

        assert!(
            calls.load(Ordering::SeqCst) >= 3,
            "expected at least 3 reconnect attempts, got {}",
            calls.load(Ordering::SeqCst)
        );
    }
}
