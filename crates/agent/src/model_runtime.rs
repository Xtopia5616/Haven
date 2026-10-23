//! Read-only Agent view over the shared app model/media publication point.
//! Snapshots outlive swaps.
use std::sync::Arc;

use haven_common::config::MediaConfig;
#[cfg(test)]
use haven_llm::ModelMediaPublisher;
use haven_llm::{LlmRouter, ModelMediaRuntime, ModelMediaSnapshot};

#[derive(Clone)]
pub(crate) struct ModelRuntime {
    current: ModelMediaRuntime,
    #[cfg(test)]
    publisher: Option<Arc<ModelMediaPublisher>>,
}

impl ModelRuntime {
    pub(crate) fn new(router: Arc<LlmRouter>) -> Self {
        let (current, publisher) =
            ModelMediaRuntime::new(ModelMediaSnapshot::new(0, router, MediaConfig::default()));
        #[cfg(not(test))]
        drop(publisher);
        Self {
            current,
            #[cfg(test)]
            publisher: Some(Arc::new(publisher)),
        }
    }

    pub(crate) fn from_shared(current: ModelMediaRuntime) -> Self {
        Self {
            current,
            #[cfg(test)]
            publisher: None,
        }
    }

    pub(crate) fn snapshot(&self) -> Arc<LlmRouter> {
        self.versioned_snapshot().1
    }

    /// The generation and router are captured from the same immutable bundle.
    pub(crate) fn versioned_snapshot(&self) -> (u64, Arc<LlmRouter>) {
        let snapshot = self.current.snapshot();
        (snapshot.generation(), snapshot.router().clone())
    }

    #[cfg(test)]
    pub(crate) fn publish_router_for_test(&self, router: Arc<LlmRouter>) -> anyhow::Result<()> {
        let old = self.current.snapshot();
        self.publisher
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("test model runtime has no publisher capability"))?
            .publish(
                ModelMediaSnapshot::new(old.config_version(), router, old.media_config().clone())
                    .with_clients(
                        old.stt_client().cloned(),
                        old.ocr_client().cloned(),
                        old.image_gen_client().cloned(),
                        old.tts_client().cloned(),
                    ),
            )?;
        Ok(())
    }
}
