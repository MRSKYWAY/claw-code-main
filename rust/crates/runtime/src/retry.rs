use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDisposition {
    Retry,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl RetryPolicy {
    #[must_use]
    pub const fn new(
        max_retries: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        Self {
            max_retries,
            initial_backoff,
            max_backoff,
        }
    }

    #[must_use]
    pub const fn max_retries(&self) -> u32 {
        self.max_retries
    }

    #[must_use]
    pub const fn disposition(&self, retry_count: u32) -> RetryDisposition {
        if retry_count < self.max_retries {
            RetryDisposition::Retry
        } else {
            RetryDisposition::Stop
        }
    }

    #[must_use]
    pub fn backoff(&self, retry_count: u32) -> Duration {
        let multiplier = 1u128 << retry_count.min(20);
        let millis = self.initial_backoff.as_millis().saturating_mul(multiplier);
        let bounded = millis.min(self.max_backoff.as_millis());
        Duration::from_millis(bounded.min(u128::from(u64::MAX)) as u64)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(2, Duration::from_millis(200), Duration::from_secs(2))
    }
}

#[cfg(test)]
mod tests {
    use super::{RetryDisposition, RetryPolicy};
    use std::time::Duration;

    #[test]
    fn stops_after_configured_retry_count() {
        let policy = RetryPolicy::new(2, Duration::from_millis(100), Duration::from_secs(1));

        assert_eq!(policy.disposition(0), RetryDisposition::Retry);
        assert_eq!(policy.disposition(1), RetryDisposition::Retry);
        assert_eq!(policy.disposition(2), RetryDisposition::Stop);
    }

    #[test]
    fn caps_exponential_backoff() {
        let policy = RetryPolicy::new(5, Duration::from_millis(200), Duration::from_millis(500));

        assert_eq!(policy.backoff(0), Duration::from_millis(200));
        assert_eq!(policy.backoff(1), Duration::from_millis(400));
        assert_eq!(policy.backoff(2), Duration::from_millis(500));
        assert_eq!(policy.backoff(20), Duration::from_millis(500));
    }
}
