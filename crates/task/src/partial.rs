use haven_memory::Database;
use std::collections::HashMap;
use std::sync::Arc;

/// Single coordination point for a task's checkpointed stream text
/// (`partial_messages` scratch table).
///
/// Every write, promote and discard goes through this store so the lifecycle
/// cannot drift:
///
/// - **checkpoint** — the ReAct loop persists accumulated streamed text while
///   an LLM response is in flight (crash/stop recovery). A checkpoint is a
///   generation-tagged write: the caller captures the task's generation
///   before spawning the async write, and the write is dropped if a
///   promote/discard bumped the generation in the meantime. Without this a
///   late checkpoint could land AFTER a promote and re-create the row —
///   leading to a duplicated message on the next promote or a permanent
///   orphan row.
/// - **promote** — task end (user stop / crash finalize): the row is
///   atomically taken and inserted as a real assistant message, unless a
///   newer real message already supersedes it.
/// - **discard** — the partial is obsolete (a real message was persisted, a
///   retry/rollback is re-streaming, or the step failed and the error path
///   persisted the buffers as real messages).
///
/// All operations are serialized PER TASK by a per-task async lock (they are
/// fast: the DB work happens in one blocking round trip), so concurrent
/// checkpoints on DIFFERENT tasks never contend with each other, and a
/// checkpoint and a promote/discard on the SAME task can never interleave.
pub struct PartialStore {
    db: Arc<Database>,
    /// Per-task async mutex, keyed by task id. Only tasks sharing a task id
    /// serialize against each other, so a slow checkpoint on one task does
    /// not stall promote/discard on other tasks (no cross-task head-of-line
    /// blocking).
    locks: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Per-task generation counter, bumped on every promote/discard. A
    /// checkpoint captures it before its (possibly queued) write and re-checks
    /// under the task lock; a stale write is dropped.
    generation: std::sync::Mutex<HashMap<String, u64>>,
    /// Last checkpointed content per task: an unchanged snapshot skips the
    /// write entirely (the time throttle alone would otherwise rewrite the
    /// same row every interval while a slow model streams nothing new).
    last_written: std::sync::Mutex<HashMap<String, String>>,
}

impl PartialStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            locks: tokio::sync::Mutex::new(HashMap::new()),
            generation: std::sync::Mutex::new(HashMap::new()),
            last_written: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Acquire the per-task serialization lock for `task_id`. This serializes
    /// only against other operations on the SAME task.
    async fn task_lock(&self, task_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut map = self.locks.lock().await;
            map.entry(task_id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    /// Current generation of a task's partial stream. Callers capture this
    /// BEFORE spawning an async checkpoint write and pass it to
    /// [`Self::checkpoint`].
    pub fn generation(&self, task_id: &str) -> u64 {
        self.generation
            .lock()
            .unwrap()
            .get(task_id)
            .copied()
            .unwrap_or(0)
    }

    /// Persist streamed text for a task. `gen_id` must be the generation
    /// captured before the write was spawned; if a promote/discard has
    /// happened since, the write is dropped as stale. Skips writes whose
    /// content is unchanged since the last checkpoint.
    pub async fn checkpoint(&self, task_id: &str, gen_id: u64, content: &str) {
        if content.trim().is_empty() {
            return;
        }
        let _guard = self.task_lock(task_id).await;
        if gen_id != self.generation(task_id) {
            return;
        }
        if self
            .last_written
            .lock()
            .unwrap()
            .get(task_id)
            .is_some_and(|prev| prev == content)
        {
            return;
        }
        let db = self.db.clone();
        let tid = task_id.to_string();
        let snapshot = content.to_string();
        if let Err(e) = db
            .run_blocking(move |db| db.upsert_partial_message(&tid, &snapshot))
            .await
        {
            tracing::warn!(
                "PartialStore: failed to checkpoint stream text for task {}: {}",
                task_id,
                e
            );
            return;
        }
        self.last_written
            .lock()
            .unwrap()
            .insert(task_id.to_string(), content.to_string());
    }

    /// Promote the checkpointed text into a real assistant message (task
    /// end / crash finalize). Also invalidates any in-flight checkpoint so
    /// it cannot re-create the row afterwards. Returns `true` when a message
    /// was inserted.
    pub async fn promote(&self, task_id: &str) -> anyhow::Result<bool> {
        let _guard = self.task_lock(task_id).await;
        self.bump_generation(task_id);
        self.last_written.lock().unwrap().remove(task_id);
        // The task is ending: drop its lock entry so the map does not grow
        // unboundedly across a long-running process. The generation bump above
        // already invalidates any in-flight checkpoint; a future checkpoint
        // re-creates a fresh lock on its next write.
        self.locks.lock().await.remove(task_id);
        let db = self.db.clone();
        let tid = task_id.to_string();
        db.run_blocking(move |db| db.promote_partial_message(&tid))
            .await
    }

    /// Drop the checkpointed text (superseded by real messages, retry/
    /// rollback re-stream, or a failed step whose buffers were persisted by
    /// the error path). Invalidates in-flight checkpoints.
    pub async fn discard(&self, task_id: &str) {
        let _guard = self.task_lock(task_id).await;
        self.bump_generation(task_id);
        self.last_written.lock().unwrap().remove(task_id);
        self.locks.lock().await.remove(task_id);
        let db = self.db.clone();
        let tid = task_id.to_string();
        let _ = db
            .run_blocking(move |db| db.delete_partial_message(&tid))
            .await;
    }

    fn bump_generation(&self, task_id: &str) {
        let mut gen_id = self.generation.lock().unwrap();
        let next = gen_id.get(task_id).copied().unwrap_or(0).wrapping_add(1);
        gen_id.insert(task_id.to_string(), next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_store() -> (PartialStore, Arc<Database>, tempfile::TempDir, String) {
        let dir = tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.db")).unwrap());
        let task = db.create_task("input", "").unwrap();
        let task_id = task.id.clone();
        (PartialStore::new(db.clone()), db, dir, task_id)
    }

    #[tokio::test]
    async fn checkpoint_promote_cycle() {
        let (store, _db, _dir, task_id) = test_store();
        let gen_id = store.generation(&task_id);
        store.checkpoint(&task_id, gen_id, "streamed text").await;
        store.checkpoint(&task_id, gen_id, "streamed text").await; // unchanged → skipped
        assert!(store.promote(&task_id).await.unwrap());
        assert!(!store.promote(&task_id).await.unwrap());
    }

    #[tokio::test]
    async fn stale_checkpoint_after_discard_is_dropped() {
        let (store, db, _dir, task_id) = test_store();
        let gen_id = store.generation(&task_id);
        store.checkpoint(&task_id, gen_id, "first draft").await;
        // A promote/discard bumps the generation; a checkpoint spawned before
        // it must not re-create the row.
        store.discard(&task_id).await;
        store.checkpoint(&task_id, gen_id, "first draft").await;
        let tid = task_id.clone();
        let row = db
            .run_blocking(move |db| Ok(db.get_partial_message(&tid)))
            .await
            .unwrap();
        assert!(row.is_none(), "stale checkpoint must not re-create the row");
    }

    #[tokio::test]
    async fn promote_then_stale_checkpoint_does_not_duplicate() {
        let (store, db, _dir, task_id) = test_store();
        let gen_id = store.generation(&task_id);
        store.checkpoint(&task_id, gen_id, "partial reply").await;
        assert!(store.promote(&task_id).await.unwrap());
        // The in-flight checkpoint (same generation as before promote) lands
        // afterwards and must be dropped.
        store.checkpoint(&task_id, gen_id, "partial reply").await;
        let tid = task_id.clone();
        let msgs = db
            .run_blocking(move |db| db.get_task_messages(&tid))
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1, "promoted message must not be duplicated");
        let tid2 = task_id;
        let row = db
            .run_blocking(move |db| Ok(db.get_partial_message(&tid2)))
            .await
            .unwrap();
        assert!(
            row.is_none(),
            "stale checkpoint must not leave an orphan row"
        );
    }
}
