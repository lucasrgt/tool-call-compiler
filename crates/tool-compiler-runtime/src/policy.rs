//! Retry, timeout, and single-flight policies around adapter calls.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Mutex;
use tool_compiler_adapter_api::{BatchInput, BatchOutput, ToolExecutionError, ToolExecutor};
use tool_compiler_ir::Effects;

use crate::cache::CacheKey;

const DEFAULT_BACKOFF_MS: u64 = 100;
const MAX_BACKOFF_MS: u64 = 5_000;

/// Per-call execution policy derived from effects and run configuration.
#[derive(Clone, Debug, Default)]
pub(crate) struct CallPolicy {
    pub timeout: Option<Duration>,
    pub max_attempts: u8,
    pub backoff_ms: u64,
    pub retryable_errors: Vec<String>,
}

impl CallPolicy {
    pub(crate) fn from_effects(effects: Option<&Effects>, default_timeout_ms: Option<u64>) -> Self {
        let timeout_ms = effects
            .and_then(|effects| effects.timeout_ms)
            .or(default_timeout_ms);
        let retry_allowed = effects.is_some_and(|effects| effects.idempotent || effects.pure);
        let retry = effects.and_then(|effects| effects.retry.as_ref());

        Self {
            timeout: timeout_ms.map(Duration::from_millis),
            max_attempts: match (retry_allowed, retry) {
                (true, Some(policy)) => policy.max_attempts.max(1),
                _ => 1,
            },
            backoff_ms: retry
                .and_then(|policy| policy.backoff_ms)
                .unwrap_or(DEFAULT_BACKOFF_MS),
            retryable_errors: retry
                .map(|policy| policy.retryable_errors.iter().cloned().collect())
                .unwrap_or_default(),
        }
    }

    fn is_retryable(&self, error: &ToolExecutionError) -> bool {
        if let Some(verdict) = error.retryable {
            return verdict;
        }
        if self.retryable_errors.is_empty() {
            return true;
        }
        self.retryable_errors.iter().any(|needle| {
            error.code.as_deref() == Some(needle.as_str()) || error.message.contains(needle)
        })
    }

    fn backoff(&self, attempt: u8) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let base = self
            .backoff_ms
            .saturating_mul(1u64 << exponent)
            .min(MAX_BACKOFF_MS);
        // Jitter without a rand dependency: the wall clock's sub-second
        // nanoseconds are effectively random across concurrent retries.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| u64::from(elapsed.subsec_nanos()))
            .unwrap_or(0);
        let jitter = nanos % (base / 4 + 1);
        Duration::from_millis(base + jitter)
    }
}

/// The outcome of a policy-wrapped call plus how many retries it took.
pub(crate) struct PolicyOutcome<T> {
    pub result: Result<T, ToolExecutionError>,
    pub retries: Vec<ToolExecutionError>,
}

pub(crate) async fn call_with_policy(
    executor: &Arc<dyn ToolExecutor>,
    tool: &str,
    input: &Value,
    policy: &CallPolicy,
) -> PolicyOutcome<Value> {
    run_attempts(policy, || {
        let executor = executor.clone();
        let tool = tool.to_owned();
        let input = input.clone();
        async move { with_timeout(policy.timeout, executor.call(&tool, input)).await }
    })
    .await
}

pub(crate) async fn call_batch_with_policy(
    executor: &Arc<dyn ToolExecutor>,
    tool: &str,
    inputs: &[BatchInput],
    policy: &CallPolicy,
) -> PolicyOutcome<Vec<BatchOutput>> {
    run_attempts(policy, || {
        let executor = executor.clone();
        let tool = tool.to_owned();
        let inputs = inputs.to_vec();
        async move { with_timeout(policy.timeout, executor.call_batch(&tool, inputs)).await }
    })
    .await
}

async fn run_attempts<T, F, Fut>(policy: &CallPolicy, mut attempt_fn: F) -> PolicyOutcome<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ToolExecutionError>>,
{
    let mut retries = Vec::new();
    let mut attempt = 1u8;

    loop {
        match attempt_fn().await {
            Ok(value) => {
                return PolicyOutcome {
                    result: Ok(value),
                    retries,
                };
            }
            Err(error) => {
                if attempt >= policy.max_attempts || !policy.is_retryable(&error) {
                    return PolicyOutcome {
                        result: Err(error),
                        retries,
                    };
                }
                tokio::time::sleep(policy.backoff(attempt)).await;
                retries.push(error);
                attempt += 1;
            }
        }
    }
}

async fn with_timeout<T>(
    timeout: Option<Duration>,
    future: impl Future<Output = Result<T, ToolExecutionError>>,
) -> Result<T, ToolExecutionError> {
    match timeout {
        Some(limit) => match tokio::time::timeout(limit, future).await {
            Ok(result) => result,
            Err(_) => Err(ToolExecutionError::new(format!(
                "timed out after {}ms",
                limit.as_millis()
            ))
            .with_code("timeout")),
        },
        None => future.await,
    }
}

/// Coalesces concurrent executions of the same cache key: the first caller
/// executes while later callers wait and then re-check the cache.
///
/// Slots are reference-counted and removed when the last guard drops, so the
/// table only ever holds keys that are actually in flight (a long-lived
/// runtime does not accumulate one entry per distinct call).
#[derive(Default)]
pub(crate) struct SingleFlight {
    inflight: std::sync::Mutex<HashMap<CacheKey, Slot>>,
}

struct Slot {
    lock: Arc<Mutex<()>>,
    guards: usize,
}

impl SingleFlight {
    /// Returns a guard that serializes callers of `key`. Dropping the guard
    /// releases the slot (and frees it once no caller holds it).
    pub(crate) async fn acquire(flight: &Arc<Self>, key: &CacheKey) -> SingleFlightGuard {
        let lock = {
            let mut inflight = flight.lock_table();
            let slot = inflight.entry(key.clone()).or_insert_with(|| Slot {
                lock: Arc::new(Mutex::new(())),
                guards: 0,
            });
            slot.guards += 1;
            Arc::clone(&slot.lock)
        };
        let permit = lock.lock_owned().await;
        SingleFlightGuard {
            flight: Arc::clone(flight),
            key: key.clone(),
            _permit: permit,
        }
    }

    fn lock_table(&self) -> std::sync::MutexGuard<'_, HashMap<CacheKey, Slot>> {
        self.inflight
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

pub(crate) struct SingleFlightGuard {
    flight: Arc<SingleFlight>,
    key: CacheKey,
    _permit: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for SingleFlightGuard {
    fn drop(&mut self) {
        let mut inflight = self.flight.lock_table();
        if let Some(slot) = inflight.get_mut(&self.key) {
            slot.guards -= 1;
            if slot.guards == 0 {
                inflight.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;
    use tool_compiler_ir::RetryPolicy;

    use super::*;

    struct FlakyExecutor {
        calls: AtomicUsize,
        succeed_at: usize,
    }

    #[async_trait]
    impl ToolExecutor for FlakyExecutor {
        async fn call(&self, _tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call >= self.succeed_at {
                Ok(input)
            } else {
                Err(ToolExecutionError::new("flaky").with_code("unavailable"))
            }
        }
    }

    fn retrying_effects(max_attempts: u8) -> Effects {
        Effects {
            retry: Some(RetryPolicy {
                max_attempts,
                retryable_errors: Default::default(),
                backoff_ms: Some(1),
            }),
            ..Effects::pure()
        }
    }

    #[tokio::test]
    async fn retries_until_success_when_idempotent() {
        let executor: Arc<dyn ToolExecutor> = Arc::new(FlakyExecutor {
            calls: AtomicUsize::new(0),
            succeed_at: 3,
        });
        let policy = CallPolicy::from_effects(Some(&retrying_effects(5)), None);

        let outcome = call_with_policy(&executor, "t", &json!(1), &policy).await;

        assert_eq!(outcome.result.unwrap(), json!(1));
        assert_eq!(outcome.retries.len(), 2);
    }

    #[tokio::test]
    async fn does_not_retry_non_idempotent_tools() {
        let executor: Arc<dyn ToolExecutor> = Arc::new(FlakyExecutor {
            calls: AtomicUsize::new(0),
            succeed_at: 2,
        });
        let effects = Effects {
            idempotent: false,
            pure: false,
            retry: Some(RetryPolicy {
                max_attempts: 5,
                retryable_errors: Default::default(),
                backoff_ms: Some(1),
            }),
            ..Effects::default()
        };
        let policy = CallPolicy::from_effects(Some(&effects), None);

        let outcome = call_with_policy(&executor, "t", &json!(1), &policy).await;

        assert!(outcome.result.is_err());
        assert!(outcome.retries.is_empty());
    }

    #[tokio::test]
    async fn respects_retryable_error_filters() {
        let executor: Arc<dyn ToolExecutor> = Arc::new(FlakyExecutor {
            calls: AtomicUsize::new(0),
            succeed_at: 2,
        });
        let mut effects = retrying_effects(5);
        effects.retry.as_mut().unwrap().retryable_errors = ["other_code".to_owned()].into();
        let policy = CallPolicy::from_effects(Some(&effects), None);

        let outcome = call_with_policy(&executor, "t", &json!(1), &policy).await;

        assert!(outcome.result.is_err());
    }

    #[tokio::test]
    async fn adapter_retryable_verdict_wins() {
        struct NeverRetry;
        #[async_trait]
        impl ToolExecutor for NeverRetry {
            async fn call(&self, _tool: &str, _input: Value) -> Result<Value, ToolExecutionError> {
                Err(ToolExecutionError::new("permanent").with_retryable(false))
            }
        }
        let executor: Arc<dyn ToolExecutor> = Arc::new(NeverRetry);
        let policy = CallPolicy::from_effects(Some(&retrying_effects(5)), None);

        let outcome = call_with_policy(&executor, "t", &json!(1), &policy).await;

        assert!(outcome.result.is_err());
        assert!(outcome.retries.is_empty());
    }

    #[tokio::test]
    async fn timeouts_produce_timeout_errors() {
        struct SlowExecutor;
        #[async_trait]
        impl ToolExecutor for SlowExecutor {
            async fn call(&self, _tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(input)
            }
        }
        let executor: Arc<dyn ToolExecutor> = Arc::new(SlowExecutor);
        let mut effects = Effects::pure();
        effects.timeout_ms = Some(5);
        let policy = CallPolicy::from_effects(Some(&effects), None);

        let outcome = call_with_policy(&executor, "t", &json!(1), &policy).await;

        let error = outcome.result.unwrap_err();
        assert_eq!(error.code.as_deref(), Some("timeout"));
    }
}
