//! One immutable publication unit for the model router and its media clients.
//!
//! Consumers capture a snapshot once per operation. Replacing the current
//! snapshot never mutates values already held by an in-flight request.

use std::sync::{Arc, RwLock};

use haven_common::config::MediaConfig;

use crate::{ImageGenClient, LlmRouter, OcrClient, SttClient, TtsClient};

#[derive(Clone)]
pub struct ModelMediaSnapshot {
    /// Process-local publication sequence; not a persistent entity identity.
    generation: u64,
    /// Version of the durable ConfigService snapshot used to prepare this
    /// generation.
    config_version: u64,
    router: Arc<LlmRouter>,
    stt_client: Option<Arc<dyn SttClient>>,
    ocr_client: Option<Arc<dyn OcrClient>>,
    image_gen_client: Option<Arc<dyn ImageGenClient>>,
    tts_client: Option<Arc<dyn TtsClient>>,
    media_config: MediaConfig,
}

impl ModelMediaSnapshot {
    pub fn new(config_version: u64, router: Arc<LlmRouter>, media_config: MediaConfig) -> Self {
        Self {
            generation: 0,
            config_version,
            router,
            stt_client: None,
            ocr_client: None,
            image_gen_client: None,
            tts_client: None,
            media_config,
        }
    }

    pub fn with_clients(
        mut self,
        stt_client: Option<Arc<dyn SttClient>>,
        ocr_client: Option<Arc<dyn OcrClient>>,
        image_gen_client: Option<Arc<dyn ImageGenClient>>,
        tts_client: Option<Arc<dyn TtsClient>>,
    ) -> Self {
        self.stt_client = stt_client;
        self.ocr_client = ocr_client;
        self.image_gen_client = image_gen_client;
        self.tts_client = tts_client;
        self
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn config_version(&self) -> u64 {
        self.config_version
    }

    pub fn router(&self) -> &Arc<LlmRouter> {
        &self.router
    }

    pub fn stt_client(&self) -> Option<&Arc<dyn SttClient>> {
        self.stt_client.as_ref()
    }

    pub fn ocr_client(&self) -> Option<&Arc<dyn OcrClient>> {
        self.ocr_client.as_ref()
    }

    pub fn image_gen_client(&self) -> Option<&Arc<dyn ImageGenClient>> {
        self.image_gen_client.as_ref()
    }

    pub fn tts_client(&self) -> Option<&Arc<dyn TtsClient>> {
        self.tts_client.as_ref()
    }

    pub fn media_config(&self) -> &MediaConfig {
        &self.media_config
    }

    fn with_generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }
}

/// Shared application handle to the current model/media generation.
#[derive(Clone)]
pub struct ModelMediaRuntime {
    current: Arc<RwLock<Arc<ModelMediaSnapshot>>>,
}

impl ModelMediaRuntime {
    /// Create a read handle and its composition-owner-only publisher.
    pub fn new(snapshot: ModelMediaSnapshot) -> (Self, ModelMediaPublisher) {
        let current = Arc::new(RwLock::new(Arc::new(snapshot.with_generation(0))));
        (
            Self {
                current: current.clone(),
            },
            ModelMediaPublisher { current },
        )
    }

    /// Capture one immutable generation. A caller can keep this `Arc` across
    /// awaits and publication without observing a partially changed bundle.
    pub fn snapshot(&self) -> Arc<ModelMediaSnapshot> {
        self.current
            .read()
            .expect("model/media runtime lock poisoned")
            .clone()
    }
}

/// The sole mutation capability for a model/media runtime. Keep it in the
/// composition root; Agent, Memory, and Tools receive only read handles.
pub struct ModelMediaPublisher {
    current: Arc<RwLock<Arc<ModelMediaSnapshot>>>,
}

impl ModelMediaPublisher {
    /// Atomically publish a fully prepared model/media generation.
    pub fn publish(&self, snapshot: ModelMediaSnapshot) -> anyhow::Result<u64> {
        let mut current = self
            .current
            .write()
            .map_err(|_| anyhow::anyhow!("model/media runtime lock is poisoned"))?;
        let generation = current
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("model/media runtime generation exhausted"))?;
        *current = Arc::new(snapshot.with_generation(generation));
        Ok(generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::RouterConfig;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn snapshot(router: Arc<LlmRouter>, timeout_secs: u64) -> ModelMediaSnapshot {
        let mut config = MediaConfig::default();
        config.stt.timeout_secs = timeout_secs;
        ModelMediaSnapshot::new(timeout_secs, router, config)
    }

    #[test]
    fn publication_swaps_the_complete_generation_and_preserves_old_snapshots() {
        let router_a = Arc::new(LlmRouter::new(RouterConfig::default()));
        let router_b = Arc::new(LlmRouter::new(RouterConfig::default()));
        let (runtime, publisher) = ModelMediaRuntime::new(snapshot(router_a.clone(), 11));
        let old = runtime.snapshot();

        assert_eq!(
            publisher.publish(snapshot(router_b.clone(), 22)).unwrap(),
            1
        );

        let current = runtime.snapshot();
        assert_eq!(old.generation(), 0);
        assert_eq!(old.config_version(), 11);
        assert_eq!(old.media_config().stt.timeout_secs, 11);
        assert!(Arc::ptr_eq(old.router(), &router_a));
        assert_eq!(current.generation(), 1);
        assert_eq!(current.config_version(), 22);
        assert_eq!(current.media_config().stt.timeout_secs, 22);
        assert!(Arc::ptr_eq(current.router(), &router_b));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_readers_only_observe_complete_model_media_generations() {
        let router_a = Arc::new(LlmRouter::new(RouterConfig::default()));
        let router_b = Arc::new(LlmRouter::new(RouterConfig::default()));
        let (runtime, publisher) = ModelMediaRuntime::new(snapshot(router_a.clone(), 11));
        let finished = Arc::new(AtomicBool::new(false));

        let writer_publisher = publisher;
        let writer_router_a = router_a.clone();
        let writer_router_b = router_b.clone();
        let writer_finished = finished.clone();
        let writer = tokio::spawn(async move {
            for version in 0..500 {
                let (router, timeout) = if version % 2 == 0 {
                    (writer_router_b.clone(), 22)
                } else {
                    (writer_router_a.clone(), 11)
                };
                writer_publisher.publish(snapshot(router, timeout)).unwrap();
            }
            writer_finished.store(true, Ordering::Release);
        });

        let mut readers = Vec::new();
        for _ in 0..4 {
            let runtime = runtime.clone();
            let finished = finished.clone();
            let router_a = router_a.clone();
            let router_b = router_b.clone();
            readers.push(tokio::spawn(async move {
                while !finished.load(Ordering::Acquire) {
                    let snapshot = runtime.snapshot();
                    let is_a = Arc::ptr_eq(snapshot.router(), &router_a);
                    let is_b = Arc::ptr_eq(snapshot.router(), &router_b);
                    assert_ne!(is_a, is_b);
                    assert_eq!(
                        snapshot.media_config().stt.timeout_secs,
                        if is_a { 11 } else { 22 }
                    );
                }
            }));
        }

        writer.await.unwrap();
        for reader in readers {
            reader.await.unwrap();
        }
    }
}
