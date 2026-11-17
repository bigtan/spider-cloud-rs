use rand::RngExt;
use std::time::Duration;

/// Compute exponential backoff (`base_ms * 2^attempt`, capped at `max_ms`) with ±20% random jitter.
pub fn backoff_with_jitter(attempt: u32, base_ms: u64, max_ms: u64) -> Duration {
    if base_ms == 0 || max_ms == 0 {
        return Duration::ZERO;
    }

    let multiplier = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    let capped_ms = base_ms.saturating_mul(multiplier).min(max_ms);
    let factor: f64 = rand::rng().random_range(0.8_f64..=1.2_f64);
    let jittered_ms = ((capped_ms as f64) * factor).round().max(0.0) as u64;
    Duration::from_millis(jittered_ms)
}

/// Return `true` for HTTP status codes worth retrying (`408`, `429`, and `5xx`).
pub fn is_retryable_http_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500..=599)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_stays_within_expected_jitter_bounds() {
        for _ in 0..50 {
            let d0 = backoff_with_jitter(0, 100, 1000);
            assert!(d0 >= Duration::from_millis(80) && d0 <= Duration::from_millis(120));

            let dcapped = backoff_with_jitter(10, 100, 1000);
            assert!(
                dcapped >= Duration::from_millis(800) && dcapped <= Duration::from_millis(1200)
            );
        }

        assert!(backoff_with_jitter(u32::MAX, 100, 5000).as_millis() <= 6000);
        assert_eq!(backoff_with_jitter(0, 0, 1000), Duration::ZERO);
        assert_eq!(backoff_with_jitter(0, 100, 0), Duration::ZERO);
    }

    #[test]
    fn retryable_http_status_matches_408_429_5xx_only() {
        assert!(is_retryable_http_status(408));
        assert!(is_retryable_http_status(429));
        assert!(is_retryable_http_status(500));
        assert!(is_retryable_http_status(503));
        assert!(is_retryable_http_status(599));

        assert!(!is_retryable_http_status(200));
        assert!(!is_retryable_http_status(400));
        assert!(!is_retryable_http_status(401));
        assert!(!is_retryable_http_status(403));
        assert!(!is_retryable_http_status(404));
    }
}
