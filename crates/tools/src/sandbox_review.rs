//! Bounded owner for completed Windows AppContainer output reviews.
//!
//! The owner reserves capacity before execution, accounts for active and
//! pending work together, and drops expired filesystem owners outside its
//! registry lock. The model-facing Shell surface never receives this owner.

use async_trait::async_trait;
use haven_common::types::is_canonical_id;
use haven_platform::appcontainer_process::AppContainerOutputReview;
use haven_platform::filesystem::{DirectoryTreeChangeKind, DirectoryTreeOutputManifest};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_PENDING_REVIEW_COUNT: usize = 8;
const MAX_PENDING_REVIEW_BYTES: u64 = 512 * 1024 * 1024;
const PENDING_REVIEW_TTL: Duration = Duration::from_secs(15 * 60);
const PENDING_REVIEW_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// User-review metadata for one completed sandbox run. File content and
/// absolute paths remain private to the Tools owner.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SandboxOutputReviewManifest {
    pub session_id: String,
    pub source_step_id: String,
    pub expires_at_unix_ms: u64,
    pub storage_charge_bytes: u64,
    pub changes: Vec<SandboxOutputReviewChange>,
}

/// One path-level change in the bounded AppContainer output manifest.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SandboxOutputReviewChange {
    /// Stable index into this immutable manifest; promotion APIs use the
    /// index instead of trusting a renderer-supplied source path.
    pub change_index: u32,
    /// Display-only path. The exact path is resolved from the owner-held
    /// manifest when a user selects a change.
    pub relative_path: String,
    pub change: SandboxOutputReviewChangeKind,
    pub before_bytes: Option<u64>,
    pub after_bytes: Option<u64>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxOutputReviewChangeKind {
    Created,
    Modified,
    Deleted,
    TypeChanged,
}

/// App-facing capability for retaining and viewing short-lived sandbox
/// reviews. Promotion is intentionally added by the separate authorization
/// owner path; this port never returns the underlying review object.
#[async_trait]
pub trait SandboxOutputReviewPort: Send + Sync {
    async fn reserve(
        &self,
        session_id: &str,
        source_step_id: &str,
        maximum_storage_charge_bytes: u64,
    ) -> Result<Box<dyn SandboxOutputReviewReservation>, String>;

    async fn manifest(
        &self,
        session_id: &str,
        source_step_id: &str,
    ) -> Result<Option<SandboxOutputReviewManifest>, String>;

    async fn discard(&self, session_id: &str, source_step_id: &str) -> Result<bool, String>;
}

/// One active Shell attempt's storage reservation. If the run fails or is
/// cancelled, dropping this value releases its count and byte charge.
pub trait SandboxOutputReviewReservation: Send {
    fn retain(self: Box<Self>, review: AppContainerOutputReview) -> Result<(), String>;
}

#[derive(Clone)]
pub(crate) struct SandboxOutputReviewOwner {
    inner: Arc<ReviewOwnerInner>,
}

struct ReviewOwnerInner {
    state: Mutex<ReviewOwnerState>,
    expiry_worker_started: AtomicBool,
    max_review_count: usize,
    max_review_bytes: u64,
    review_ttl: Duration,
}

#[derive(Default)]
struct ReviewOwnerState {
    reserved: HashMap<ReviewKey, u64>,
    pending: HashMap<ReviewKey, PendingReview>,
    charged_bytes: u64,
}

struct PendingReview {
    review: Arc<Mutex<AppContainerOutputReview>>,
    expires_at: Instant,
    storage_charge_bytes: u64,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ReviewKey {
    session_id: String,
    source_step_id: String,
}

struct Reservation {
    owner: Weak<ReviewOwnerInner>,
    key: ReviewKey,
    reserved_bytes: u64,
    active: bool,
}

impl Default for SandboxOutputReviewOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxOutputReviewOwner {
    pub(crate) fn new() -> Self {
        Self::with_limits(
            MAX_PENDING_REVIEW_COUNT,
            MAX_PENDING_REVIEW_BYTES,
            PENDING_REVIEW_TTL,
        )
    }

    fn with_limits(max_review_count: usize, max_review_bytes: u64, review_ttl: Duration) -> Self {
        Self {
            inner: Arc::new(ReviewOwnerInner {
                state: Mutex::new(ReviewOwnerState::default()),
                expiry_worker_started: AtomicBool::new(false),
                max_review_count,
                max_review_bytes,
                review_ttl,
            }),
        }
    }

    pub(crate) fn start_expiry_worker(&self) {
        if self
            .inner
            .expiry_worker_started
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PENDING_REVIEW_SWEEP_INTERVAL).await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let owner = SandboxOutputReviewOwner { inner };
                owner.sweep_expired().await;
            }
        });
    }

    async fn sweep_expired(&self) {
        let expired = match self.inner.state.lock() {
            Ok(mut state) => state.remove_expired(Instant::now()),
            Err(_) => {
                tracing::error!("sandbox review owner lock is poisoned; expiry sweep skipped");
                return;
            }
        };
        if expired.is_empty() {
            return;
        }
        if let Err(error) = tokio::task::spawn_blocking(move || drop(expired)).await {
            tracing::warn!(
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "sandbox review expiry cleanup worker failed"
            );
        }
    }

    fn key(session_id: &str, source_step_id: &str) -> Result<ReviewKey, String> {
        if !is_canonical_id(session_id, "ses") {
            return Err("session_id must be a canonical ses- id".into());
        }
        if !is_canonical_id(source_step_id, "step") {
            return Err("source_step_id must be a canonical step- id".into());
        }
        Ok(ReviewKey {
            session_id: session_id.to_owned(),
            source_step_id: source_step_id.to_owned(),
        })
    }
}

#[async_trait]
impl SandboxOutputReviewPort for SandboxOutputReviewOwner {
    async fn reserve(
        &self,
        session_id: &str,
        source_step_id: &str,
        maximum_storage_charge_bytes: u64,
    ) -> Result<Box<dyn SandboxOutputReviewReservation>, String> {
        let key = Self::key(session_id, source_step_id)?;
        let (reserved, expired) = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| "sandbox review owner is unavailable".to_string())?;
            let expired = state.remove_expired(Instant::now());
            let reserved = state.reserve(
                key.clone(),
                maximum_storage_charge_bytes,
                self.inner.max_review_count,
                self.inner.max_review_bytes,
            );
            (reserved, expired)
        };
        drop_expired(expired).await;
        reserved?;
        Ok(Box::new(Reservation {
            owner: Arc::downgrade(&self.inner),
            key,
            reserved_bytes: maximum_storage_charge_bytes,
            active: true,
        }))
    }

    async fn manifest(
        &self,
        session_id: &str,
        source_step_id: &str,
    ) -> Result<Option<SandboxOutputReviewManifest>, String> {
        let key = Self::key(session_id, source_step_id)?;
        let (review, charge, expires_at, expired) = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| "sandbox review owner is unavailable".to_string())?;
            let expired = state.remove_expired(Instant::now());
            let pending = state.pending.get(&key);
            (
                pending.map(|entry| Arc::clone(&entry.review)),
                pending.map(|entry| entry.storage_charge_bytes),
                pending.map(|entry| entry.expires_at),
                expired,
            )
        };
        drop_expired(expired).await;
        let (Some(review), Some(charge), Some(expires_at)) = (review, charge, expires_at) else {
            return Ok(None);
        };
        let review = review
            .lock()
            .map_err(|_| "sandbox output review is unavailable".to_string())?;
        Ok(Some(manifest_snapshot(
            &key,
            review.manifest(),
            charge,
            expires_at,
        )))
    }

    async fn discard(&self, session_id: &str, source_step_id: &str) -> Result<bool, String> {
        let key = Self::key(session_id, source_step_id)?;
        let (discarded, expired) = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| "sandbox review owner is unavailable".to_string())?;
            let expired = state.remove_expired(Instant::now());
            let discarded = state.remove_pending(&key);
            (discarded, expired)
        };
        drop_expired(expired).await;
        if let Some(discarded) = discarded {
            drop_expired(vec![discarded]).await;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl ReviewOwnerState {
    fn reserve(
        &mut self,
        key: ReviewKey,
        bytes: u64,
        max_review_count: usize,
        max_review_bytes: u64,
    ) -> Result<(), String> {
        if self.reserved.contains_key(&key) || self.pending.contains_key(&key) {
            return Err("sandbox output review already exists for this tool step".into());
        }
        if bytes > max_review_bytes {
            return Err("sandbox output review exceeds the per-process storage budget".into());
        }
        if self.reserved.len().saturating_add(self.pending.len()) >= max_review_count {
            return Err("sandbox output review capacity is exhausted".into());
        }
        let next_charge = self
            .charged_bytes
            .checked_add(bytes)
            .ok_or_else(|| "sandbox output review storage budget overflowed".to_string())?;
        if next_charge > max_review_bytes {
            return Err("sandbox output review storage capacity is exhausted".into());
        }
        self.charged_bytes = next_charge;
        self.reserved.insert(key, bytes);
        Ok(())
    }

    fn retain(
        &mut self,
        key: ReviewKey,
        reserved_bytes: u64,
        storage_charge_bytes: u64,
    ) -> Result<(), String> {
        if self.pending.contains_key(&key) {
            return Err("sandbox output review already exists for this tool step".into());
        }
        if self.reserved.get(&key).copied() != Some(reserved_bytes) {
            return Err("sandbox output review reservation is no longer active".into());
        }
        if storage_charge_bytes > reserved_bytes {
            return Err("sandbox output review exceeded its reserved storage charge".into());
        }
        self.reserved.remove(&key);
        self.charged_bytes -= reserved_bytes - storage_charge_bytes;
        Ok(())
    }

    fn remove_pending(&mut self, key: &ReviewKey) -> Option<PendingReview> {
        let pending = self.pending.remove(key)?;
        self.charged_bytes -= pending.storage_charge_bytes;
        Some(pending)
    }

    fn remove_expired(&mut self, now: Instant) -> Vec<PendingReview> {
        let keys = self
            .pending
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.iter()
            .filter_map(|key| self.remove_pending(key))
            .collect()
    }
}

impl Reservation {
    fn release(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let Ok(mut state) = owner.state.lock() else {
            tracing::error!("sandbox review owner lock is poisoned; reservation release skipped");
            return;
        };
        if let Some(bytes) = state.reserved.remove(&self.key) {
            state.charged_bytes -= bytes;
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.release();
    }
}

impl SandboxOutputReviewReservation for Reservation {
    fn retain(mut self: Box<Self>, review: AppContainerOutputReview) -> Result<(), String> {
        let storage_charge_bytes = review.retained_storage_upper_bound_bytes();
        let expires_at = Instant::now()
            + self
                .owner
                .upgrade()
                .map_or(PENDING_REVIEW_TTL, |owner| owner.review_ttl);
        let review = Arc::new(Mutex::new(review));
        let Some(owner) = self.owner.upgrade() else {
            return Err("sandbox output review owner has shut down".into());
        };
        let result = match owner.state.lock() {
            Err(_) => Err("sandbox review owner is unavailable".into()),
            Ok(mut state) => state
                .retain(self.key.clone(), self.reserved_bytes, storage_charge_bytes)
                .map(|()| {
                    state.pending.insert(
                        self.key.clone(),
                        PendingReview {
                            review: Arc::clone(&review),
                            expires_at,
                            storage_charge_bytes,
                        },
                    );
                }),
        };
        if result.is_ok() {
            self.active = false;
        }
        drop(review);
        result
    }
}

async fn drop_expired(expired: Vec<PendingReview>) {
    if expired.is_empty() {
        return;
    }
    if let Err(error) = tokio::task::spawn_blocking(move || drop(expired)).await {
        tracing::warn!(
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "sandbox output review cleanup worker failed"
        );
    }
}

fn manifest_snapshot(
    key: &ReviewKey,
    manifest: &DirectoryTreeOutputManifest,
    storage_charge_bytes: u64,
    expires_at: Instant,
) -> SandboxOutputReviewManifest {
    let remaining = expires_at.saturating_duration_since(Instant::now());
    let expiry_system_time = SystemTime::now()
        .checked_add(remaining)
        .unwrap_or(UNIX_EPOCH);
    let expires_at_unix_ms = expiry_system_time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    SandboxOutputReviewManifest {
        session_id: key.session_id.clone(),
        source_step_id: key.source_step_id.clone(),
        expires_at_unix_ms,
        storage_charge_bytes,
        changes: manifest
            .changes
            .iter()
            .enumerate()
            .map(|(index, change)| SandboxOutputReviewChange {
                change_index: u32::try_from(index).unwrap_or(u32::MAX),
                relative_path: change.relative_path.to_string_lossy().into_owned(),
                change: match change.change {
                    DirectoryTreeChangeKind::Created => SandboxOutputReviewChangeKind::Created,
                    DirectoryTreeChangeKind::Modified => SandboxOutputReviewChangeKind::Modified,
                    DirectoryTreeChangeKind::Deleted => SandboxOutputReviewChangeKind::Deleted,
                    DirectoryTreeChangeKind::TypeChanged => {
                        SandboxOutputReviewChangeKind::TypeChanged
                    }
                },
                before_bytes: change.before_bytes,
                after_bytes: change.after_bytes,
                before_sha256: change.before_sha256.map(hex_digest),
                after_sha256: change.after_sha256.map(hex_digest),
            })
            .collect(),
    }
}

fn hex_digest(digest: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::new_id;
    use haven_platform::appcontainer_process::AppContainerCommand;
    use haven_platform::filesystem::{
        DirectoryCapability, DirectoryTreeCopyLimits, DirectoryTreeDiffLimits,
    };
    use haven_platform::process_environment::filter_shell_environment;
    use std::io::Read;
    use std::time::Duration;

    #[tokio::test]
    async fn owner_accounts_active_and_pending_storage_and_expires_reviews() {
        let (review, sandbox_root) = create_review().await;
        let charge = review.retained_storage_upper_bound_bytes();
        let session_id = new_id("ses");
        let source_step_id = new_id("step");
        let owner = SandboxOutputReviewOwner::with_limits(1, charge, Duration::from_millis(30));

        let reservation = owner
            .reserve(&session_id, &source_step_id, charge)
            .await
            .expect("reserve bounded storage before retaining output");
        assert!(
            owner
                .reserve(&new_id("ses"), &new_id("step"), 0)
                .await
                .is_err(),
            "active reservations count toward the pending-review limit"
        );
        reservation
            .retain(review)
            .expect("retain the completed review within its reservation");

        let manifest = owner
            .manifest(&session_id, &source_step_id)
            .await
            .unwrap()
            .expect("retained review is queryable by its existing session and step ids");
        assert_eq!(manifest.storage_charge_bytes, charge);
        assert_eq!(manifest.changes.len(), 1);
        assert_eq!(manifest.changes[0].relative_path, "review-output.txt");
        assert_eq!(
            manifest.changes[0].change,
            SandboxOutputReviewChangeKind::Created
        );
        assert!(sandbox_root.exists());

        tokio::time::sleep(Duration::from_millis(40)).await;
        owner.sweep_expired().await;
        assert!(
            owner
                .manifest(&session_id, &source_step_id)
                .await
                .unwrap()
                .is_none(),
            "expired reviews are no longer visible"
        );
        assert!(
            !sandbox_root.exists(),
            "expiry drops the RAII workspace owner"
        );
    }

    #[tokio::test]
    async fn reservation_drop_and_discard_release_capacity() {
        let owner = SandboxOutputReviewOwner::with_limits(1, 10, Duration::from_secs(60));
        let first_session = new_id("ses");
        let first_step = new_id("step");
        let reservation = owner
            .reserve(&first_session, &first_step, 10)
            .await
            .unwrap();
        drop(reservation);
        owner
            .reserve(&new_id("ses"), &new_id("step"), 10)
            .await
            .expect("dropping an unfinished run releases its reserved capacity");

        let (review, _) = create_review().await;
        let charge = review.retained_storage_upper_bound_bytes();
        let owner = SandboxOutputReviewOwner::with_limits(1, charge, Duration::from_secs(60));
        owner
            .reserve(&first_session, &first_step, charge)
            .await
            .unwrap()
            .retain(review)
            .unwrap();
        assert!(owner.discard(&first_session, &first_step).await.unwrap());
        assert!(!owner.discard(&first_session, &first_step).await.unwrap());
        owner
            .reserve(&new_id("ses"), &new_id("step"), charge)
            .await
            .expect("explicit discard releases review count and byte charge");
    }

    #[tokio::test]
    async fn reservations_validate_ids_and_enforce_aggregate_bytes() {
        let owner = SandboxOutputReviewOwner::with_limits(2, 10, Duration::from_secs(60));
        assert!(
            owner
                .reserve("ses-invalid", &new_id("step"), 1)
                .await
                .is_err(),
            "noncanonical session ids cannot key an owner entry"
        );

        let first_session = new_id("ses");
        let first_step = new_id("step");
        let first = owner
            .reserve(&first_session, &first_step, 6)
            .await
            .expect("reserve the first bounded attempt");
        assert!(
            owner.reserve(&first_session, &first_step, 1).await.is_err(),
            "one session step cannot own two simultaneous reviews"
        );
        assert!(
            owner
                .reserve(&new_id("ses"), &new_id("step"), 5)
                .await
                .is_err(),
            "active plus pending reservation bytes cannot exceed the aggregate budget"
        );
        drop(first);

        owner
            .reserve(&new_id("ses"), &new_id("step"), 10)
            .await
            .expect("released capacity can be reserved again");
        assert!(
            owner
                .reserve(&new_id("ses"), &new_id("step"), 11)
                .await
                .is_err(),
            "a single attempt cannot reserve more than the aggregate hard limit"
        );
    }

    async fn create_review() -> (AppContainerOutputReview, std::path::PathBuf) {
        let fixture = tempfile::tempdir().expect("create isolated review fixture");
        let source_root = fixture.path().join("source");
        std::fs::create_dir(&source_root).expect("create snapshot source");
        let executable_source = std::env::current_exe().expect("locate the test executable");
        std::fs::copy(&executable_source, source_root.join("review-probe.exe"))
            .expect("copy the AppContainer review child into the private snapshot");
        let source = DirectoryCapability::open(&source_root).unwrap();
        let copy_limits =
            DirectoryTreeCopyLimits::new(16, 4, 512 * 1024 * 1024, 1024 * 1024 * 1024).unwrap();
        let workspace = haven_platform::windows_sandbox::AppContainerWorkspace::create(
            &source,
            7,
            11,
            copy_limits,
            || false,
        )
        .expect("provision a bounded private AppContainer workspace");
        let sandbox_root = workspace.root().to_path_buf();
        let executable = sandbox_root.join("review-probe.exe");
        let command = AppContainerCommand::new(executable).args([
            "--exact",
            "sandbox_review::tests::appcontainer_review_child",
            "--nocapture",
            "--",
            "--sandbox-review-child",
        ]);
        let command = command.envs(filter_shell_environment(std::env::vars_os()));
        let mut child = command
            .spawn(workspace)
            .expect("start the review probe in AppContainer");
        let deadline = Instant::now() + Duration::from_secs(25);
        while child
            .wait_timeout(Duration::from_millis(100))
            .expect("poll the review probe")
            .is_none()
        {
            assert!(
                Instant::now() < deadline,
                "AppContainer review probe timed out"
            );
        }
        for mut stream in [child.take_stdout(), child.take_stderr()]
            .into_iter()
            .flatten()
        {
            let mut sink = Vec::new();
            stream.read_to_end(&mut sink).unwrap();
        }
        let diff_limits = DirectoryTreeDiffLimits::new(copy_limits, 16, 1024, 4096).unwrap();
        let review = child
            .take_output_review(diff_limits, || false)
            .expect("collect the bounded review after process tree exit");
        drop(child);
        (review, sandbox_root)
    }

    #[test]
    fn appcontainer_review_child() {
        if std::env::args().any(|argument| argument == "--sandbox-review-child") {
            std::fs::write("review-output.txt", "bounded-review")
                .expect("write the one sandbox output file");
        }
    }
}
