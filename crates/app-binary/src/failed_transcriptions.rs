//! Durable, bounded retention for recordings whose transcription did not
//! finish. A failed WAV stays available for an explicit retry across restart.

use crate::recording_lifecycle::RecordingTargetSnapshot;
use haven_common::types::{is_canonical_id, new_id};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

const MAX_FAILED_TRANSCRIPTIONS: usize = 10;
const MAX_FAILED_TRANSCRIPTION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FAILED_TRANSCRIPTION_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct FailedTranscriptionStore {
    root: Arc<PathBuf>,
    gate: Arc<Mutex<()>>,
    active_retries: Arc<StdMutex<HashSet<String>>>,
}

pub(crate) struct FailedTranscriptionRetryLease {
    retry_file_id: String,
    active_retries: Arc<StdMutex<HashSet<String>>>,
}

impl Drop for FailedTranscriptionRetryLease {
    fn drop(&mut self) {
        self.active_retries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.retry_file_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FailedTranscriptionMetadata {
    pub(crate) retry_file_id: String,
    pub(crate) created_at_ms: u64,
    pub(crate) duration_ms: u64,
    pub(crate) target_session_id: Option<String>,
    pub(crate) selection_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedTranscriptionSummary {
    pub retry_file_id: String,
    pub created_at_ms: u64,
    pub duration_ms: u64,
}

pub(crate) struct FailedTranscriptionData {
    pub(crate) metadata: FailedTranscriptionMetadata,
    pub(crate) wav: Vec<u8>,
}

impl FailedTranscriptionStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
            gate: Arc::new(Mutex::new(())),
            active_retries: Arc::new(StdMutex::new(HashSet::new())),
        }
    }

    pub(crate) fn production() -> Self {
        Self::new(haven_common::config::ConfigLoader::data_dir().join("failed-transcriptions"))
    }

    pub(crate) async fn recover_orphans(&self) -> std::io::Result<usize> {
        let _guard = self.gate.lock().await;
        recover_orphans_at(&self.root).await
    }

    pub(crate) async fn save(
        &self,
        wav: &[u8],
        duration_ms: u64,
        target: &RecordingTargetSnapshot,
    ) -> anyhow::Result<FailedTranscriptionMetadata> {
        anyhow::ensure!(!wav.is_empty(), "cannot retain an empty recording");
        anyhow::ensure!(
            wav.len() as u64 <= MAX_FAILED_TRANSCRIPTION_FILE_BYTES,
            "recording exceeds the failed-transcription retention limit"
        );
        let _guard = self.gate.lock().await;
        // Saving a newly failed recording can race the renderer's startup
        // listing. Reap incomplete prior writes under the same store gate so
        // an orphan sidecar cannot make `list_at` reject this recording and
        // cause the just-failed audio to be lost.
        recover_orphans_at(&self.root).await?;
        let existing = list_at(&self.root).await?;
        anyhow::ensure!(
            existing.len() < MAX_FAILED_TRANSCRIPTIONS,
            "failed-transcription retention is full"
        );
        let used_bytes = existing.iter().try_fold(0_u64, |total, item| {
            let (wav_path, _) = paths(&self.root, &item.retry_file_id);
            Ok::<_, std::io::Error>(total.saturating_add(std::fs::metadata(wav_path)?.len()))
        })?;
        anyhow::ensure!(
            used_bytes.saturating_add(wav.len() as u64) <= MAX_FAILED_TRANSCRIPTION_BYTES,
            "failed-transcription storage limit is full"
        );

        tokio::fs::create_dir_all(&*self.root).await?;
        let retry_file_id = new_id("file");
        let metadata = FailedTranscriptionMetadata {
            retry_file_id: retry_file_id.clone(),
            created_at_ms: now_ms(),
            duration_ms,
            target_session_id: target.session_id.clone(),
            selection_version: target.selection_version,
        };
        let (wav_path, metadata_path) = paths(&self.root, &retry_file_id);
        let (wav_tmp, metadata_tmp) = temp_paths(&self.root, &retry_file_id);
        tokio::fs::write(&wav_tmp, wav).await?;
        if let Err(error) = tokio::fs::rename(&wav_tmp, &wav_path).await {
            let _ = tokio::fs::remove_file(&wav_tmp).await;
            return Err(error.into());
        }
        let serialized = match serde_json::to_vec(&metadata) {
            Ok(serialized) => serialized,
            Err(error) => {
                let _ = tokio::fs::remove_file(&wav_path).await;
                return Err(error.into());
            }
        };
        if let Err(error) = tokio::fs::write(&metadata_tmp, serialized).await {
            let _ = tokio::fs::remove_file(&wav_path).await;
            let _ = tokio::fs::remove_file(&metadata_tmp).await;
            return Err(error.into());
        }
        if let Err(error) = tokio::fs::rename(&metadata_tmp, &metadata_path).await {
            let _ = tokio::fs::remove_file(&wav_path).await;
            let _ = tokio::fs::remove_file(&metadata_tmp).await;
            return Err(error.into());
        }
        Ok(metadata)
    }

    pub(crate) async fn list(&self) -> anyhow::Result<Vec<FailedTranscriptionSummary>> {
        let _guard = self.gate.lock().await;
        list_at(&self.root).await
    }

    pub(crate) async fn load(
        &self,
        retry_file_id: &str,
    ) -> anyhow::Result<FailedTranscriptionData> {
        validate_retry_id(retry_file_id)?;
        let _guard = self.gate.lock().await;
        let (wav_path, metadata_path) = paths(&self.root, retry_file_id);
        let metadata: FailedTranscriptionMetadata =
            serde_json::from_slice(&tokio::fs::read(metadata_path).await?)?;
        anyhow::ensure!(
            metadata.retry_file_id == retry_file_id,
            "failed-transcription metadata identity mismatch"
        );
        if let Some(session_id) = metadata.target_session_id.as_deref() {
            anyhow::ensure!(
                is_canonical_id(session_id, "ses"),
                "invalid retained session id"
            );
        }
        let len = tokio::fs::metadata(&wav_path).await?.len();
        anyhow::ensure!(
            len > 0 && len <= MAX_FAILED_TRANSCRIPTION_FILE_BYTES,
            "retained recording has an invalid size"
        );
        let wav = tokio::fs::read(wav_path).await?;
        Ok(FailedTranscriptionData { metadata, wav })
    }

    /// Claim one retained recording for retry. The lease lives through the STT
    /// task so repeated clicks or duplicate IPC calls cannot launch the same
    /// source audio concurrently.
    pub(crate) fn claim_retry(
        &self,
        retry_file_id: &str,
    ) -> anyhow::Result<Option<FailedTranscriptionRetryLease>> {
        validate_retry_id(retry_file_id)?;
        let mut active = self
            .active_retries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !active.insert(retry_file_id.to_owned()) {
            return Ok(None);
        }
        Ok(Some(FailedTranscriptionRetryLease {
            retry_file_id: retry_file_id.to_owned(),
            active_retries: Arc::clone(&self.active_retries),
        }))
    }

    pub(crate) async fn remove(&self, retry_file_id: &str) -> anyhow::Result<bool> {
        validate_retry_id(retry_file_id)?;
        let _guard = self.gate.lock().await;
        let (wav_path, metadata_path) = paths(&self.root, retry_file_id);
        let wav_removed = remove_if_exists(&wav_path).await?;
        let metadata_removed = remove_if_exists(&metadata_path).await?;
        Ok(wav_removed || metadata_removed)
    }
}

async fn list_at(root: &Path) -> anyhow::Result<Vec<FailedTranscriptionSummary>> {
    let entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut summaries = Vec::new();
    let mut entries = entries;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let metadata: FailedTranscriptionMetadata =
            serde_json::from_slice(&tokio::fs::read(&path).await?)?;
        validate_retry_id(&metadata.retry_file_id)?;
        let (wav_path, _) = paths(root, &metadata.retry_file_id);
        let file_metadata = tokio::fs::metadata(wav_path).await?;
        anyhow::ensure!(
            file_metadata.is_file()
                && file_metadata.len() > 0
                && file_metadata.len() <= MAX_FAILED_TRANSCRIPTION_FILE_BYTES,
            "retained recording has an invalid size"
        );
        if let Some(session_id) = metadata.target_session_id.as_deref() {
            anyhow::ensure!(
                is_canonical_id(session_id, "ses"),
                "invalid retained session id"
            );
        }
        summaries.push(FailedTranscriptionSummary {
            retry_file_id: metadata.retry_file_id,
            created_at_ms: metadata.created_at_ms,
            duration_ms: metadata.duration_ms,
        });
    }
    summaries.sort_by_key(|item| item.created_at_ms);
    Ok(summaries)
}

async fn recover_orphans_at(root: &Path) -> std::io::Result<usize> {
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    let present: std::collections::HashSet<_> = names.iter().cloned().collect();
    let mut removed = 0;
    for name in names {
        let path = root.join(&name);
        let orphan = if name.starts_with(".file-") && name.ends_with(".tmp") {
            true
        } else if let Some(id) = name.strip_suffix(".wav") {
            !present.contains(&format!("{id}.json"))
        } else if let Some(id) = name.strip_suffix(".json") {
            !present.contains(&format!("{id}.wav"))
        } else {
            false
        };
        if orphan {
            tokio::fs::remove_file(path).await?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn validate_retry_id(retry_file_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(retry_file_id, "file"),
        "invalid retry file identity"
    );
    Ok(())
}

fn paths(root: &Path, retry_file_id: &str) -> (PathBuf, PathBuf) {
    (
        root.join(format!("{retry_file_id}.wav")),
        root.join(format!("{retry_file_id}.json")),
    )
}

fn temp_paths(root: &Path, retry_file_id: &str) -> (PathBuf, PathBuf) {
    (
        root.join(format!(".{retry_file_id}.wav.tmp")),
        root.join(format!(".{retry_file_id}.json.tmp")),
    )
}

async fn remove_if_exists(path: &Path) -> std::io::Result<bool> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::FailedTranscriptionStore;
    use crate::recording_lifecycle::RecordingTargetSnapshot;

    #[tokio::test]
    async fn retained_failures_survive_reopen_and_are_removed_after_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let store = FailedTranscriptionStore::new(temp.path().to_path_buf());
        let target = RecordingTargetSnapshot {
            session_id: Some("ses-0123456789abcdef0123456789abcdef".into()),
            selection_version: 7,
            revision: 1,
        };
        let saved = store.save(b"wav fixture", 1200, &target).await.unwrap();
        drop(store);
        let reopened = FailedTranscriptionStore::new(temp.path().to_path_buf());
        let pending = reopened.list().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].retry_file_id, saved.retry_file_id);
        let loaded = reopened.load(&saved.retry_file_id).await.unwrap();
        assert_eq!(loaded.wav, b"wav fixture");
        assert_eq!(loaded.metadata.target_session_id, target.session_id);
        assert_eq!(loaded.metadata.selection_version, 7);
        assert!(reopened.remove(&saved.retry_file_id).await.unwrap());
        assert!(reopened.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn startup_cleanup_removes_orphan_files_and_staging_but_keeps_pairs() {
        let temp = tempfile::tempdir().unwrap();
        let store = FailedTranscriptionStore::new(temp.path().to_path_buf());
        let target = RecordingTargetSnapshot::default();
        let saved = store.save(b"wav fixture", 10, &target).await.unwrap();
        tokio::fs::write(
            temp.path()
                .join("file-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.wav"),
            b"orphan",
        )
        .await
        .unwrap();
        tokio::fs::write(
            temp.path()
                .join(".file-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.wav.tmp"),
            b"stage",
        )
        .await
        .unwrap();

        assert_eq!(store.recover_orphans().await.unwrap(), 2);
        assert_eq!(store.list().await.unwrap().len(), 1);
        assert!(store.load(&saved.retry_file_id).await.is_ok());
    }

    #[tokio::test]
    async fn saving_a_failure_cleans_orphans_before_reading_retained_records() {
        let temp = tempfile::tempdir().unwrap();
        let store = FailedTranscriptionStore::new(temp.path().to_path_buf());
        let orphan_id = "file-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let orphan_json = temp.path().join(format!("{orphan_id}.json"));
        tokio::fs::write(
            &orphan_json,
            serde_json::json!({
                "retry_file_id": orphan_id,
                "created_at_ms": 1,
                "duration_ms": 10,
                "target_session_id": null,
                "selection_version": 0
            })
            .to_string(),
        )
        .await
        .unwrap();
        let orphan_wav = temp
            .path()
            .join("file-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.wav");
        tokio::fs::write(&orphan_wav, b"orphan").await.unwrap();
        let staging = temp
            .path()
            .join(".file-cccccccccccccccccccccccccccccccc.json.tmp");
        tokio::fs::write(&staging, b"partial").await.unwrap();

        let saved = store
            .save(b"new retained wav", 20, &RecordingTargetSnapshot::default())
            .await
            .expect("an orphan sidecar must not block retention of the new failure");

        assert!(!orphan_json.exists());
        assert!(!orphan_wav.exists());
        assert!(!staging.exists());
        assert_eq!(store.list().await.unwrap().len(), 1);
        assert_eq!(
            store.load(&saved.retry_file_id).await.unwrap().wav,
            b"new retained wav"
        );
    }

    #[tokio::test]
    async fn retention_is_bounded_without_evicting_earlier_failures() {
        let temp = tempfile::tempdir().unwrap();
        let store = FailedTranscriptionStore::new(temp.path().to_path_buf());
        let target = RecordingTargetSnapshot::default();
        let first = store.save(b"one", 1, &target).await.unwrap();
        for _ in 1..10 {
            store.save(b"next", 1, &target).await.unwrap();
        }
        assert!(store.save(b"eleventh", 1, &target).await.is_err());
        assert_eq!(store.list().await.unwrap().len(), 10);
        assert!(store.load(&first.retry_file_id).await.is_ok());
    }

    #[tokio::test]
    async fn one_retained_recording_cannot_be_retried_concurrently() {
        let temp = tempfile::tempdir().unwrap();
        let store = FailedTranscriptionStore::new(temp.path().to_path_buf());
        let target = RecordingTargetSnapshot::default();
        let saved = store.save(b"wav fixture", 10, &target).await.unwrap();

        let first = store.claim_retry(&saved.retry_file_id).unwrap().unwrap();
        assert!(store.claim_retry(&saved.retry_file_id).unwrap().is_none());
        drop(first);
        assert!(store.claim_retry(&saved.retry_file_id).unwrap().is_some());
    }
}
