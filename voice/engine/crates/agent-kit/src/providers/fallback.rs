//! Fallback LLM provider — tries providers in order until one works.
//!
//! Eliminates dead air by cascading through backup LLMs when the
//! primary fails or times out.
//!
//! Each provider slot tracks health status. Failed providers are
//! automatically probed in the background and restored when they recover.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::providers::{LlmCallConfig, LlmProvider, LlmProviderError};
use crate::agent_backends::ChatMessage;
use crate::providers::LlmEvent;

// ── Provider Slot ───────────────────────────────────────────────

/// State of a single provider in the fallback chain.
struct ProviderSlot {
    provider: Box<dyn LlmProvider>,
    name: String,
    healthy: bool,
    last_failure: Option<Instant>,
}

// ── FallbackProvider ────────────────────────────────────────────

/// An `LlmProvider` that tries multiple backends in priority order.
///
/// If the primary fails (error or timeout), it immediately tries the
/// next healthy provider. Failed providers are probed periodically
/// and restored when they recover.
///
/// # Example
/// ```rust,ignore
/// use std::time::Duration;
///
/// let provider = FallbackProvider::new(
///     vec![
///         ("gpt-4o".into(), primary_provider),
///         ("claude-3.5".into(), fallback_provider),
///     ],
///     Duration::from_secs(5),
/// );
/// ```
pub struct FallbackProvider {
    slots: Arc<RwLock<Vec<ProviderSlot>>>,
    attempt_timeout: Duration,
    recovery_interval: Duration,
}

impl FallbackProvider {
    /// Create a new fallback provider from an ordered list of `(name, provider)` pairs.
    ///
    /// * `attempt_timeout` — max time to wait for each provider before trying the next.
    pub fn new(providers: Vec<(String, Box<dyn LlmProvider>)>, attempt_timeout: Duration) -> Self {
        let slots = providers
            .into_iter()
            .map(|(name, provider)| ProviderSlot {
                provider,
                name,
                healthy: true,
                last_failure: None,
            })
            .collect();

        Self {
            slots: Arc::new(RwLock::new(slots)),
            attempt_timeout,
            recovery_interval: Duration::from_secs(30),
        }
    }

    /// Override the default recovery probe interval (default: 30s).
    pub fn with_recovery_interval(mut self, interval: Duration) -> Self {
        self.recovery_interval = interval;
        self
    }

    /// Spawn a background task that periodically checks if a failed
    /// provider has recovered, and restores it if so.
    fn spawn_recovery_probe(&self, slot_index: usize) {
        let slots = Arc::clone(&self.slots);
        let interval = self.recovery_interval;

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;

                let should_probe = {
                    let slots_r = slots.read().await;
                    // Slot was restored by another path, or doesn't exist
                    match slots_r.get(slot_index) {
                        Some(slot) if !slot.healthy => true,
                        _ => false,
                    }
                };

                if !should_probe {
                    return; // Already healthy or gone — stop probing
                }

                // Probe: attempt a minimal completion to check if the
                // provider is reachable. We use a tiny max_tokens to
                // minimize cost.
                let probe_result = {
                    let slots_r = slots.read().await;
                    let slot = &slots_r[slot_index];
                    let messages = vec![ChatMessage {
                        role: "user".to_string(),
                        content: Some(serde_json::Value::String("ping".to_string())),
                        tool_calls: None,
                        tool_call_id: None,
                    }];
                    let config = LlmCallConfig {
                        temperature: 0.0,
                        max_tokens: 1,
                        model: None,
                    };

                    tokio::time::timeout(
                        Duration::from_secs(10),
                        slot.provider.stream_completion(&messages, None, &config),
                    )
                    .await
                };

                match probe_result {
                    Ok(Ok(_rx)) => {
                        // Provider is back!
                        let mut slots_w = slots.write().await;
                        if let Some(slot) = slots_w.get_mut(slot_index) {
                            slot.healthy = true;
                            slot.last_failure = None;
                            info!("[fallback] Provider '{}' recovered", slot.name);
                        }
                        return; // Stop probing
                    }
                    _ => {
                        let slots_r = slots.read().await;
                        if let Some(slot) = slots_r.get(slot_index) {
                            warn!("[fallback] Provider '{}' still unhealthy", slot.name);
                        }
                        // Continue probing
                    }
                }
            }
        });
    }
}

#[async_trait]
impl LlmProvider for FallbackProvider {
    async fn stream_completion(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[serde_json::Value]>,
        config: &LlmCallConfig,
    ) -> Result<mpsc::Receiver<LlmEvent>, LlmProviderError> {
        // Snapshot the current slot count and health status so we can
        // iterate cleanly without holding the lock across await points.
        let slot_count = {
            let slots = self.slots.read().await;
            slots.len()
        };

        for i in 0..slot_count {
            // Check health
            let is_healthy = {
                let slots = self.slots.read().await;
                slots.get(i).map_or(false, |s| s.healthy)
            };

            if !is_healthy {
                continue;
            }

            // Attempt the call
            let result = {
                let slots = self.slots.read().await;
                let slot = &slots[i];
                tokio::time::timeout(
                    self.attempt_timeout,
                    slot.provider.stream_completion(messages, tools, config),
                )
                .await
            };

            match result {
                Ok(Ok(rx)) => return Ok(rx),
                Ok(Err(e)) => {
                    // Provider error — mark unhealthy and try next
                    let name = {
                        let mut slots = self.slots.write().await;
                        if let Some(slot) = slots.get_mut(i) {
                            slot.healthy = false;
                            slot.last_failure = Some(Instant::now());
                            slot.name.clone()
                        } else {
                            "unknown".to_string()
                        }
                    };
                    warn!("[fallback] '{}' failed: {}, trying next", name, e);
                    self.spawn_recovery_probe(i);
                }
                Err(_timeout) => {
                    // Timeout — mark unhealthy and try next
                    let name = {
                        let mut slots = self.slots.write().await;
                        if let Some(slot) = slots.get_mut(i) {
                            slot.healthy = false;
                            slot.last_failure = Some(Instant::now());
                            slot.name.clone()
                        } else {
                            "unknown".to_string()
                        }
                    };
                    warn!(
                        "[fallback] '{}' timed out after {:?}, trying next",
                        name, self.attempt_timeout
                    );
                    self.spawn_recovery_probe(i);
                }
            }
        }

        Err(LlmProviderError::Provider(
            "all providers in fallback chain exhausted".to_string(),
        ))
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock provider that always succeeds with a single token.
    struct MockOkProvider {
        name: String,
    }

    #[async_trait]
    impl LlmProvider for MockOkProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: Option<&[serde_json::Value]>,
            _config: &LlmCallConfig,
        ) -> Result<mpsc::Receiver<LlmEvent>, LlmProviderError> {
            let (tx, rx) = mpsc::channel(1);
            let name = self.name.clone();
            tokio::spawn(async move {
                let _ = tx.send(LlmEvent::Token(format!("from:{}", name))).await;
            });
            Ok(rx)
        }
    }

    /// A mock provider that always fails.
    struct MockFailProvider;

    #[async_trait]
    impl LlmProvider for MockFailProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: Option<&[serde_json::Value]>,
            _config: &LlmCallConfig,
        ) -> Result<mpsc::Receiver<LlmEvent>, LlmProviderError> {
            Err(LlmProviderError::Provider("mock failure".to_string()))
        }
    }

    /// A mock provider that hangs forever (for timeout testing).
    struct MockHangProvider;

    #[async_trait]
    impl LlmProvider for MockHangProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: Option<&[serde_json::Value]>,
            _config: &LlmCallConfig,
        ) -> Result<mpsc::Receiver<LlmEvent>, LlmProviderError> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            unreachable!()
        }
    }

    fn test_config() -> LlmCallConfig {
        LlmCallConfig {
            temperature: 0.7,
            max_tokens: 100,
            model: None,
        }
    }

    fn test_messages() -> Vec<ChatMessage> {
        vec![ChatMessage {
            role: "user".to_string(),
            content: Some(serde_json::Value::String("hello".to_string())),
            tool_calls: None,
            tool_call_id: None,
        }]
    }

    #[tokio::test]
    async fn primary_provider_used_when_healthy() {
        let provider = FallbackProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(MockOkProvider {
                        name: "primary".into(),
                    }),
                ),
                (
                    "secondary".into(),
                    Box::new(MockOkProvider {
                        name: "secondary".into(),
                    }),
                ),
            ],
            Duration::from_secs(5),
        );

        let mut rx = provider
            .stream_completion(&test_messages(), None, &test_config())
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            LlmEvent::Token(t) => assert_eq!(t, "from:primary"),
            _ => panic!("expected Token"),
        }
    }

    #[tokio::test]
    async fn failover_on_error() {
        let provider = FallbackProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(MockFailProvider) as Box<dyn LlmProvider>,
                ),
                (
                    "backup".into(),
                    Box::new(MockOkProvider {
                        name: "backup".into(),
                    }),
                ),
            ],
            Duration::from_secs(5),
        );

        let mut rx = provider
            .stream_completion(&test_messages(), None, &test_config())
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            LlmEvent::Token(t) => assert_eq!(t, "from:backup"),
            _ => panic!("expected Token"),
        }
    }

    #[tokio::test]
    async fn failover_on_timeout() {
        let provider = FallbackProvider::new(
            vec![
                (
                    "slow".into(),
                    Box::new(MockHangProvider) as Box<dyn LlmProvider>,
                ),
                (
                    "fast".into(),
                    Box::new(MockOkProvider {
                        name: "fast".into(),
                    }),
                ),
            ],
            Duration::from_millis(100), // 100ms timeout
        );

        let mut rx = provider
            .stream_completion(&test_messages(), None, &test_config())
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            LlmEvent::Token(t) => assert_eq!(t, "from:fast"),
            _ => panic!("expected Token"),
        }
    }

    #[tokio::test]
    async fn all_providers_exhausted() {
        let provider = FallbackProvider::new(
            vec![
                (
                    "a".into(),
                    Box::new(MockFailProvider) as Box<dyn LlmProvider>,
                ),
                (
                    "b".into(),
                    Box::new(MockFailProvider) as Box<dyn LlmProvider>,
                ),
            ],
            Duration::from_secs(5),
        );

        let result = provider
            .stream_completion(&test_messages(), None, &test_config())
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            LlmProviderError::Provider(msg) => {
                assert!(msg.contains("exhausted"));
            }
            other => panic!("expected Provider error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unhealthy_provider_skipped_on_retry() {
        let provider = FallbackProvider::new(
            vec![
                (
                    "failing".into(),
                    Box::new(MockFailProvider) as Box<dyn LlmProvider>,
                ),
                (
                    "backup".into(),
                    Box::new(MockOkProvider {
                        name: "backup".into(),
                    }),
                ),
            ],
            Duration::from_secs(5),
        );

        // First call: primary fails, backup used (primary marked unhealthy)
        let _ = provider
            .stream_completion(&test_messages(), None, &test_config())
            .await
            .unwrap();

        // Second call: primary should be skipped (unhealthy), backup used directly
        let mut rx = provider
            .stream_completion(&test_messages(), None, &test_config())
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            LlmEvent::Token(t) => assert_eq!(t, "from:backup"),
            _ => panic!("expected Token"),
        }

        // Verify primary is still unhealthy
        let slots = provider.slots.read().await;
        assert!(!slots[0].healthy);
        assert!(slots[1].healthy);
    }
}
