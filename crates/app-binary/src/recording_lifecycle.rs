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
    current_target: std::sync::Mutex<Option<RecordingTargetSnapshot>>,
    selected_target: std::sync::Mutex<RecordingTargetSnapshot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RecordingTargetSnapshot {
    pub(crate) session_id: Option<String>,
    pub(crate) selection_version: u64,
    pub(crate) revision: u64,
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

    /// Reserve the stable ID for an app-owned capture before it opens. The
    /// input layer carries it through auto-stop so callbacks can be checked
    /// against the currently active recording.
    pub(crate) fn begin(
        &self,
        _guard: &RecordingLifecycleGuard<'_>,
    ) -> haven_common::types::RecordingId {
        let mut current = self
            .current_recording_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.is_none() {
            let target = self
                .selected_target
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            *self
                .current_target
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(target);
            *current = Some(haven_common::types::RecordingId::new());
        }
        current
            .as_ref()
            .expect("recording identity was reserved")
            .clone()
    }

    /// Publish the renderer's latest conversation target for the next capture.
    /// Monotonic revisions make delayed IPC calls harmless when users switch
    /// sessions quickly. An active capture keeps its own frozen snapshot.
    pub(crate) fn set_selected_target(
        &self,
        _guard: &RecordingLifecycleGuard<'_>,
        session_id: Option<String>,
        selection_version: u64,
        revision: u64,
    ) -> bool {
        let mut selected = self
            .selected_target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if revision < selected.revision
            || (revision == selected.revision
                && (session_id != selected.session_id
                    || selection_version != selected.selection_version))
        {
            return false;
        }
        *selected = RecordingTargetSnapshot {
            session_id,
            selection_version,
            revision,
        };
        true
    }

    pub(crate) fn target(
        &self,
        _guard: &RecordingLifecycleGuard<'_>,
    ) -> Option<RecordingTargetSnapshot> {
        self.current_target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    pub(crate) fn matches_current(
        &self,
        guard: &RecordingLifecycleGuard<'_>,
        expected: &haven_common::types::RecordingId,
    ) -> bool {
        self.current(guard).as_ref() == Some(expected)
    }

    /// Detach the ID while the transition guard is held, before another start
    /// can observe the pipeline's already-Pending state.
    pub(crate) fn finish(
        &self,
        _guard: &RecordingLifecycleGuard<'_>,
    ) -> Option<haven_common::types::RecordingId> {
        let recording_id = self
            .current_recording_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if recording_id.is_some() {
            self.current_target
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
        }
        recording_id
    }

    /// Detach an identity only if it still belongs to the caller's capture.
    pub(crate) fn finish_if_current(
        &self,
        _guard: &RecordingLifecycleGuard<'_>,
        expected: &haven_common::types::RecordingId,
    ) -> bool {
        let mut current = self
            .current_recording_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.as_ref() != Some(expected) {
            return false;
        }
        current.take();
        self.current_target
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        true
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

    #[tokio::test]
    async fn stale_capture_identity_cannot_match_or_detach_a_new_recording() {
        let owner = RecordingLifecycleOwner::default();
        let lifecycle = owner.lock().await;
        let stale = owner.begin(&lifecycle);
        assert!(owner.finish_if_current(&lifecycle, &stale));

        let current = owner.begin(&lifecycle);
        assert_ne!(stale, current);
        assert!(!owner.matches_current(&lifecycle, &stale));
        assert!(!owner.finish_if_current(&lifecycle, &stale));
        assert!(owner.matches_current(&lifecycle, &current));
        assert_eq!(owner.current(&lifecycle), Some(current));
    }

    #[tokio::test]
    async fn capture_freezes_the_selected_session_and_selection_version() {
        let owner = RecordingLifecycleOwner::default();
        let lifecycle = owner.lock().await;
        assert!(owner.set_selected_target(&lifecycle, Some("ses-a".into()), 4, 1));
        let recording_id = owner.begin(&lifecycle);
        assert_eq!(
            owner.target(&lifecycle).unwrap(),
            super::RecordingTargetSnapshot {
                session_id: Some("ses-a".into()),
                selection_version: 4,
                revision: 1,
            }
        );

        assert!(owner.set_selected_target(&lifecycle, Some("ses-b".into()), 5, 2));
        assert_eq!(
            owner.target(&lifecycle).unwrap().session_id.as_deref(),
            Some("ses-a"),
            "a selection change during transcription must not retarget the capture"
        );
        assert!(!owner.set_selected_target(&lifecycle, Some("ses-stale".into()), 3, 1));
        assert_eq!(owner.finish(&lifecycle), Some(recording_id.clone()));

        let next_recording_id = owner.begin(&lifecycle);
        assert_ne!(recording_id, next_recording_id);
        assert_eq!(
            owner.target(&lifecycle).unwrap().session_id.as_deref(),
            Some("ses-b")
        );
    }
}
