//! Coordinates identity and serialization for app-owned voice recordings.
//!
//! Timed `media.record` captures share the input pipeline but do not receive
//! an app recording identity and are not controlled through this lifecycle.

/// Serializes user-facing recording controls and owns their current `rec-*`
/// correlation identity.
#[derive(Default)]
pub(crate) struct RecordingLifecycleOwner {
    transition: tokio::sync::Mutex<()>,
    current_recording_id: std::sync::Mutex<Option<haven_common::types::RecordingId>>,
}

pub(crate) struct RecordingLifecycleGuard<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl RecordingLifecycleOwner {
    pub(crate) async fn lock(&self) -> RecordingLifecycleGuard<'_> {
        RecordingLifecycleGuard {
            _guard: self.transition.lock().await,
        }
    }

    /// Get the ID assigned to the active app-owned capture, minting one only
    /// after a start path has established that capture.
    pub(crate) fn begin(
        &self,
        _guard: &RecordingLifecycleGuard<'_>,
    ) -> haven_common::types::RecordingId {
        let mut current = self
            .current_recording_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        current
            .get_or_insert_with(haven_common::types::RecordingId::new)
            .clone()
    }

    pub(crate) fn current(
        &self,
        _guard: &RecordingLifecycleGuard<'_>,
    ) -> Option<haven_common::types::RecordingId> {
        self.current_recording_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Detach the ID while the transition guard is held, before another start
    /// can observe the pipeline's already-Pending state.
    pub(crate) fn finish(
        &self,
        _guard: &RecordingLifecycleGuard<'_>,
    ) -> Option<haven_common::types::RecordingId> {
        self.current_recording_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

#[cfg(test)]
mod tests {
    use super::RecordingLifecycleOwner;
    use std::sync::Arc;

    #[tokio::test]
    async fn handoff_serializes_the_next_start() {
        let owner = Arc::new(RecordingLifecycleOwner::default());
        let lifecycle = owner.lock().await;
        let first_id = owner.begin(&lifecycle);

        let next_owner = owner.clone();
        let (attempting_tx, attempting_rx) = tokio::sync::oneshot::channel();
        let next_start = tokio::spawn(async move {
            attempting_tx.send(()).unwrap();
            let lifecycle = next_owner.lock().await;
            next_owner.begin(&lifecycle)
        });

        attempting_rx.await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(owner.finish(&lifecycle), Some(first_id.clone()));
        drop(lifecycle);

        let next_id = tokio::time::timeout(std::time::Duration::from_secs(1), next_start)
            .await
            .expect("the next start should acquire the lifecycle guard")
            .unwrap();
        assert_ne!(next_id, first_id);
        assert!(next_id.as_str().starts_with("rec-"));
    }

    #[tokio::test]
    async fn concurrent_stops_detach_an_identity_only_once() {
        let owner = Arc::new(RecordingLifecycleOwner::default());
        let lifecycle = owner.lock().await;
        let recording_id = owner.begin(&lifecycle);
        drop(lifecycle);

        let stop = |owner: Arc<RecordingLifecycleOwner>| async move {
            let lifecycle = owner.lock().await;
            owner.finish(&lifecycle)
        };
        let (first, second) = tokio::join!(stop(owner.clone()), stop(owner));

        assert_eq!(
            usize::from(first.is_some()) + usize::from(second.is_some()),
            1
        );
        assert_eq!(first.or(second), Some(recording_id));
    }
}
