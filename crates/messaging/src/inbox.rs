//! Private JSONL storage adapter for cross-session messaging.
//!
//! File transport: one shared directory (`<data_dir>/inbox`) with a registry file and one JSONL
//! mailbox per agent.
//!
//! Layout:
//! - `agents.json` — registry: agent session_id → metadata (`last_seen` heartbeat)
//! - `deleted_sessions.json` — durable tombstones with only session/parent lineage
//! - `<session_id>.jsonl` — per-agent append mailbox (one JSON envelope per line)
//! - `<session_id>.jsonl.backlog` — immutable source while bounded batches drain
//! - `<session_id>.jsonl.backlog.state.json` — durable byte cursor and active batch end
//! - `<session_id>.jsonl.processing` — at most one bounded, unacked claim batch
//! - `<session_id>.archive.jsonl` — bounded delivery audit copies, appended
//!   when a claim observes messages and therefore not proof of acknowledgement
//! - `<session_id>.jsonl.quarantine.<offset>.<sha256>.row` — operator-isolated
//!   oversized source rows, retained until the session or inbox is deleted
//! - `.lock` — cross-process file mutex (Windows-safe)
//!
//! Concurrency model: every mutation (append, claim/ack, registry
//! update) runs under the `.lock` mutex; registry, cursor, and mailbox rewrites
//! use recoverable temp files. Claim promotes the older `.processing` or
//! mailbox source to an immutable backlog by rename, then materializes at most
//! 100 rows / 1 MiB into `.processing`. Its cursor advances only after every
//! ID in that batch is acked. ReAct's claim/ack path therefore keeps a bounded
//! batch until transcript projection is durable; a crash causes at-least-once
//! redelivery with stable envelope IDs. Application callers use
//! `haven_messaging::MessagingService`; this storage module stays private so
//! wire storage cannot become a second application protocol.
//! New writes are limited to 16 MiB of logical unread data per recipient;
//! pending bytes include the unconsumed backlog and append mailbox, but not
//! the processing copy. The limit is per recipient, not a global disk quota.
//!
//! Envelopes are single-line JSON per the interop format; ids are canonical
//! `msg-{uuid32}` ([`haven_common::types::new_id`]). Session IDs are canonical
//! `ses-{uuid32}` values; their restricted alphabet also makes them safe as
//! mailbox filenames.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, SecondsFormat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use haven_common::types::new_id;

use crate::contract::{
    MAX_MESSAGE_ENVELOPE_BYTES, MAX_MESSAGE_HISTORY_LIMIT, is_expired, validate_message_envelope,
    validate_message_envelope_for_delivery, validate_message_id, validate_session_id,
};

/// Heartbeat threshold: agents whose `last_seen` is older than this are
/// reported `offline` by [`InboxBus::list_agents`] (entries are never
/// deleted — history is kept).
const OFFLINE_AFTER: Duration = Duration::from_secs(300);

/// Wait between OS lock acquisition retries.
const LOCK_WAIT: Duration = Duration::from_millis(20);
/// Upper bound for waiting on a held OS lock before failing the operation.
const LOCK_TIMEOUT: Duration = Duration::from_secs(15);
/// Archive tail scanned for duplicate ids when archiving (crash recovery:
/// a message archived but not deleted from `.processing` must not be
/// archived twice).
/// Hard byte budget for one agent's durable archive. The archive is an audit
/// tail, not an unbounded event log; the newest complete JSONL records are
/// retained when the budget is exceeded.
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024;
/// Logical unread byte budget per recipient, including the unconsumed portion
/// of an immutable backlog and the append-only mailbox. The active processing
/// batch is a copy of backlog rows and is not double-counted.
const MAX_PENDING_BYTES: u64 = 16 * 1024 * 1024;
/// Each claim scans at most this many source rows and bytes, except that one
/// first row may exceed the batch budget to guarantee forward progress.
const MAX_CLAIM_BATCH_LINES: usize = 100;
const MAX_CLAIM_BATCH_BYTES: u64 = 1024 * 1024;
const MAX_PROCESSING_BYTES: usize = MAX_CLAIM_BATCH_BYTES as usize + 1024;
/// JSONL readers allow the canonical serialized envelope plus its newline.
const MAX_ENVELOPE_LINE_BYTES: usize = MAX_MESSAGE_ENVELOPE_BYTES + 1;
/// A quarantined source row is copied in bounded chunks and may not exceed a
/// recipient's normal logical pending budget.
const MAX_QUARANTINED_ROW_BYTES: u64 = MAX_PENDING_BYTES;
/// Keep operator quarantine evidence bounded across the shared inbox.
const MAX_QUARANTINE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BACKLOG_STATE_BYTES: usize = 4 * 1024;
/// Bound the durable lineage map so recovery never promotes or parses an
/// unbounded tombstone file. The cap still allows hundreds of thousands of
/// minimal session entries.
const MAX_DELETED_SESSIONS_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BacklogSource {
    Mailbox,
    Processing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BacklogPhase {
    Preparing,
    Ready,
    Complete,
}

/// The only cursor/claim authority for an immutable JSONL backlog. `active_end`
/// remains set until every ID in the current bounded processing batch is acked;
/// `active_attempts` lets a missing processing copy be rebuilt without changing
/// stable IDs or resetting each envelope's retry count.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BacklogState {
    source: BacklogSource,
    phase: BacklogPhase,
    cursor_bytes: u64,
    active_end_bytes: Option<u64>,
    active_attempts: u32,
}

/// Default bus root: `<data_dir>/inbox` (`%APPDATA%/haven/inbox` on Windows).
fn default_inbox_dir() -> PathBuf {
    haven_common::config::ConfigLoader::data_dir().join("inbox")
}

/// Process-wide delivery notification: every successful [`InboxBus::deliver`]
/// bumps a counter so in-process receivers (the ReAct loop's automatic inbox
/// check) can react immediately instead of only polling on a step cadence.
///
/// The notifier is a [`watch`] channel holding a monotonic version. A sender
/// never blocks (bounded history: receivers only ever see the latest value),
/// which is exactly right for a "something arrived, go read it" signal.
/// All `default_root()` buses share one notifier via a process-wide singleton,
/// while test buses get a private one per root.
#[derive(Debug)]
struct InboxNotifier {
    tx: watch::Sender<u64>,
}

impl InboxNotifier {
    fn new() -> (Self, watch::Receiver<u64>) {
        let (tx, rx) = watch::channel(0);
        (Self { tx }, rx)
    }

    /// Signal "a message was just written to some mailbox". Never blocks.
    pub fn notify(&self) {
        self.tx.send_modify(|n| *n += 1);
    }

    /// Subscribe to delivery notifications (level-triggered: the current
    /// version is immediately readable, and `changed()` resolves when a new
    /// message lands).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.tx.subscribe()
    }
}

fn shared_notifier() -> Arc<InboxNotifier> {
    static NOTIFIER: OnceLock<Arc<InboxNotifier>> = OnceLock::new();
    NOTIFIER
        .get_or_init(|| {
            let (n, _rx) = InboxNotifier::new();
            Arc::new(n)
        })
        .clone()
}

/// Envelope `type` values per the interop format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    #[default]
    Message,
    Reply,
    Broadcast,
    Request,
    System,
    /// Lightweight read receipt auto-sent to the sender when a message is
    /// drained from the recipient's mailbox (never acked itself).
    Receipt,
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MessageType::Message => "message",
            MessageType::Reply => "reply",
            MessageType::Broadcast => "broadcast",
            MessageType::Request => "request",
            MessageType::System => "system",
            MessageType::Receipt => "receipt",
        })
    }
}

/// One message envelope = one line in a mailbox JSONL file.
///
/// Required fields: `message_id` / `from_session_id` / `to_session_id` / `text` / `created_at`;
/// everything else is optional and serialized as `null` when absent (kept on the wire
/// for audit readability, matching the interop format).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Unique message id, canonical `msg-{uuid32}`.
    pub message_id: String,
    /// `message | reply | broadcast | request | system | receipt`.
    pub r#type: MessageType,
    /// Sender session ID.
    pub from_session_id: String,
    /// Canonical recipient session ID. Broadcast input is expanded into one
    /// envelope per recipient before it reaches this durable field.
    pub to_session_id: String,
    /// Session id replies should target; defaults to `from_session_id` when absent.
    pub reply_to_session_id: Option<String>,
    /// When this envelope is a reply: the id of the original message.
    pub in_reply_to_message_id: Option<String>,
    pub subject: Option<String>,
    pub text: String,
    /// Optional structured payload (JSON only; files referenced by path).
    pub payload: Option<serde_json::Value>,
    /// RFC3339 creation time.
    pub created_at: String,
    /// Optional RFC3339 expiry: expired messages are not returned by
    /// [`crate::MessagingService::claim`] (still archived for audit).
    pub expires_at: Option<String>,
    /// Number of durable delivery attempts for this message. The id remains
    /// stable across retries; this field is transport delivery state, not a
    /// new message identity.
    pub delivery_attempt: u32,
}

impl Envelope {
    pub fn new(from_session_id: &str, to_session_id: &str, text: &str) -> Self {
        Self {
            message_id: new_id("msg"),
            r#type: MessageType::Message,
            from_session_id: from_session_id.into(),
            to_session_id: to_session_id.into(),
            reply_to_session_id: None,
            in_reply_to_message_id: None,
            subject: None,
            text: text.into(),
            payload: None,
            created_at: now_rfc3339(),
            expires_at: None,
            delivery_attempt: 0,
        }
    }

    /// The address a reply to this message should target: its custom
    /// `reply_to_session_id` when set, else its sender.
    pub fn reply_target_session_id(&self) -> &str {
        self.reply_to_session_id
            .as_deref()
            .unwrap_or(&self.from_session_id)
    }
}

/// Registry entry for one agent (`agents.json` value).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentEntry {
    pub session_id: String,
    /// RFC3339 heartbeat timestamp.
    pub last_seen: String,
    /// RFC3339 first-registration timestamp.
    pub started_at: String,
    /// Optional human-readable session title (shown by agents_list/UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional role label for discovery (e.g. `researcher`, `coder`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Optional parent session ID when this session was spawned by another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub capabilities: Vec<String>,
}

/// Minimal durable identity retained after explicit session deletion. The map
/// key is the deleted session id; only the parent link is needed to preserve
/// lineage through deleted nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeletedSessionEntry {
    #[serde(default)]
    parent_session_id: Option<String>,
}

/// Liveness status computed from the heartbeat, never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Online,
    Offline,
}

/// Agent view returned by [`InboxBus::list_agents`].
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub session_id: String,
    pub last_seen: String,
    pub started_at: String,
    pub status: AgentStatus,
    pub title: Option<String>,
    pub role: Option<String>,
    pub parent_session_id: Option<String>,
    pub capabilities: Vec<String>,
}

/// Result of delivering one envelope to one recipient.
#[derive(Debug, Clone, Serialize)]
pub struct SendOutcome {
    pub to_session_id: String,
    /// Whether the envelope was appended to the recipient's mailbox.
    /// Lenient delivery: `true` when the recipient mailbox exists even if its
    /// registry entry is absent or its heartbeat is stale. Deleted-session
    /// tombstones always reject delivery.
    pub delivered: bool,
    /// Recipient liveness at delivery time.
    pub status: AgentStatus,
}

/// Metadata for the next oversized row blocking one recipient's backlog.
/// The digest covers the exact source bytes, including a trailing newline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OversizedInboxRow {
    pub offset_bytes: u64,
    pub row_bytes: u64,
    pub sha256: String,
}

/// Durable result of moving a blocking row into operator quarantine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedInboxRow {
    pub row: OversizedInboxRow,
    pub quarantine_file: PathBuf,
}

/// The shared message bus. All operations are synchronous file I/O under the
/// `.lock` mutex; async callers should wrap them in `spawn_blocking`.
#[derive(Debug, Clone)]
pub(crate) struct InboxBus {
    root: PathBuf,
    notifier: Arc<InboxNotifier>,
    #[cfg(test)]
    fail_archive_append: Arc<std::sync::atomic::AtomicBool>,
}

impl InboxBus {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        let (notifier, _rx) = InboxNotifier::new();
        Self {
            root: root.into(),
            notifier: Arc::new(notifier),
            #[cfg(test)]
            fail_archive_append: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Bus rooted at `<data_dir>/inbox`, sharing the process-wide notifier so
    /// delivery notifications reach every in-process consumer.
    pub fn default_root() -> Self {
        Self {
            root: default_inbox_dir(),
            notifier: shared_notifier(),
            #[cfg(test)]
            fail_archive_append: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    fn root(&self) -> &Path {
        &self.root
    }

    /// Subscribe to delivery notifications for this bus.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.notifier.subscribe()
    }

    fn mailbox(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl"))
    }

    fn mailbox_tmp(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl.tmp"))
    }

    fn archive(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.archive.jsonl"))
    }

    fn archive_tmp(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.archive.jsonl.tmp"))
    }

    fn quarantine_row(&self, session_id: &str, row: &OversizedInboxRow) -> PathBuf {
        self.root.join(format!(
            "{session_id}.jsonl.quarantine.{}.{}.row",
            row.offset_bytes, row.sha256
        ))
    }

    fn quarantine_row_tmp(&self, session_id: &str, row: &OversizedInboxRow) -> PathBuf {
        self.quarantine_row(session_id, row)
            .with_extension("row.tmp")
    }

    fn deleted_sessions(&self) -> PathBuf {
        self.root.join("deleted_sessions.json")
    }

    fn deleted_sessions_tmp(&self) -> PathBuf {
        self.root.join("deleted_sessions.json.tmp")
    }

    fn deleted_sessions_backup(&self) -> PathBuf {
        self.root.join("deleted_sessions.json.bak")
    }

    fn processing(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl.processing"))
    }

    fn processing_tmp(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl.processing.tmp"))
    }

    fn backlog(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.jsonl.backlog"))
    }

    fn backlog_state(&self, session_id: &str) -> PathBuf {
        self.root
            .join(format!("{session_id}.jsonl.backlog.state.json"))
    }

    fn backlog_state_tmp(&self, session_id: &str) -> PathBuf {
        self.root
            .join(format!("{session_id}.jsonl.backlog.state.json.tmp"))
    }

    fn ensure_dir(&self) -> anyhow::Result<()> {
        Ok(std::fs::create_dir_all(&self.root)?)
    }

    /// Register (or re-register) the given agent: upsert its registry entry
    /// (heartbeat = "now") and make sure its mailbox file exists. Every tool
    /// call starts with this, so no separate heartbeat mechanism is needed.
    ///
    /// An empty `capabilities` slice on a re-register **preserves** the
    /// stored list (heartbeats must not wipe a previously announced profile).
    pub fn register(&self, session_id: &str, capabilities: &[String]) -> anyhow::Result<()> {
        self.register_with_profile(session_id, capabilities, None, None, None)
    }

    /// Like [`InboxBus::register`], but with an optional human-readable
    /// session title for the registry (used by the ReAct loop's heartbeat so
    /// `agents_list` and the UI can show what a session is about).
    pub fn register_with_title(
        &self,
        session_id: &str,
        capabilities: &[String],
        title: Option<&str>,
    ) -> anyhow::Result<()> {
        self.register_with_profile(session_id, capabilities, title, None, None)
    }

    /// Full profile upsert used by spawn / announce / heartbeat.
    ///
    /// On re-register:
    /// - `title` / `role` / `parent_session_id`: only overwritten when `Some`
    /// - `capabilities`: only overwritten when non-empty (empty = preserve)
    pub fn register_with_profile(
        &self,
        session_id: &str,
        capabilities: &[String],
        title: Option<&str>,
        role: Option<&str>,
        parent_session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        if let Some(p) = parent_session_id {
            validate_session_id(p)?;
        }
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        self.ensure_session_not_deleted_unlocked(session_id, "register")?;
        self.recover_mailbox_tmp_unlocked(session_id)?;
        let now = now_rfc3339();
        let mut reg = self.read_registry_unlocked()?;
        match reg.get_mut(session_id) {
            Some(e) => {
                e.last_seen = now.clone();
                if let Some(t) = title {
                    e.title = Some(t.to_string());
                }
                if let Some(r) = role {
                    e.role = if r.is_empty() {
                        None
                    } else {
                        Some(r.to_string())
                    };
                }
                if let Some(p) = parent_session_id {
                    e.parent_session_id = Some(p.to_string());
                }
                if !capabilities.is_empty() {
                    e.capabilities = capabilities.to_vec();
                }
            }
            None => {
                reg.insert(
                    session_id.into(),
                    AgentEntry {
                        session_id: session_id.into(),
                        last_seen: now.clone(),
                        started_at: now,
                        title: title.map(String::from),
                        role: role.filter(|r| !r.is_empty()).map(String::from),
                        parent_session_id: parent_session_id.map(String::from),
                        capabilities: capabilities.to_vec(),
                    },
                );
            }
        }
        self.write_registry_unlocked(&reg)?;
        // Create the mailbox if missing (the existence of this file is what
        // message_send checks to decide "agent exists").
        let mailbox = self.mailbox(session_id);
        if !mailbox.exists() {
            OpenOptions::new().create(true).append(true).open(mailbox)?;
        }
        Ok(())
    }

    /// Graceful shutdown: remove the agent from the registry. The mailbox and
    /// archive are kept, so late messages remain deliverable (reported as
    /// `offline`) and the history survives a later re-registration. Missing
    /// entries are a no-op.
    pub fn unregister(&self, session_id: &str) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let mut reg = self.read_registry_unlocked()?;
        if reg.remove(session_id).is_some() {
            self.write_registry_unlocked(&reg)?;
        }
        Ok(())
    }

    /// Retain a completed peer's metadata and parent session link while making its
    /// liveness unambiguously offline. This is used by the AgentLayer for
    /// spawned sessions so lifecycle authorization remains stable through the
    /// asynchronous terminal cleanup window.
    pub(crate) fn mark_offline(&self, session_id: &str) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        if self.is_deleted_session_unlocked(session_id)? {
            return Ok(());
        }
        let mut reg = self.read_registry_unlocked()?;
        let Some(entry) = reg.get_mut(session_id) else {
            return Ok(());
        };
        let stale_after = OFFLINE_AFTER.as_secs().saturating_add(1) as i64;
        entry.last_seen = (Local::now() - chrono::Duration::seconds(stale_after))
            .to_rfc3339_opts(SecondsFormat::Secs, false);
        self.write_registry_unlocked(&reg)?;
        Ok(())
    }

    /// Agents whose registry `parent_session_id` equals `parent_session_id` (spawned children).
    /// Filters under one lock without the online-first sort used by
    /// [`Self::list_agents`] (cascade / spawn caps care about identity, not UI order).
    pub fn list_children(&self, parent_session_id: &str) -> anyhow::Result<Vec<AgentInfo>> {
        validate_session_id(parent_session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let now = Local::now();
        let tombstones = self.read_deleted_sessions_unlocked()?;
        Ok(self
            .read_registry_unlocked()?
            .into_values()
            .filter(|e| {
                !tombstones.contains_key(&e.session_id)
                    && e.parent_session_id.as_deref() == Some(parent_session_id)
            })
            .map(|e| AgentInfo {
                status: if is_online(&e.last_seen, now) {
                    AgentStatus::Online
                } else {
                    AgentStatus::Offline
                },
                session_id: e.session_id,
                last_seen: e.last_seen,
                started_at: e.started_at,
                title: e.title,
                role: e.role,
                parent_session_id: e.parent_session_id,
                capabilities: e.capabilities,
            })
            .collect())
    }

    /// BFS descendants of `root_session_id` via registry `parent_session_id` links (cycle-safe).
    pub fn list_descendants(&self, root_session_id: &str) -> anyhow::Result<Vec<String>> {
        validate_session_id(root_session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let reg = self.read_registry_unlocked()?;
        let tombstones = self.read_deleted_sessions_unlocked()?;
        let mut by_parent: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for e in reg.values() {
            if let Some(p) = e.parent_session_id.as_deref() {
                by_parent
                    .entry(p.to_string())
                    .or_default()
                    .push(e.session_id.clone());
            }
        }
        for (session_id, tombstone) in &tombstones {
            if let Some(parent_session_id) = tombstone.parent_session_id.as_deref() {
                by_parent
                    .entry(parent_session_id.to_string())
                    .or_default()
                    .push(session_id.clone());
            }
        }
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut queue = vec![root_session_id.to_string()];
        seen.insert(root_session_id.to_string());
        while let Some(cur) = queue.pop() {
            let Some(kids) = by_parent.get(&cur) else {
                continue;
            };
            for child in kids {
                if seen.insert(child.clone()) {
                    if !tombstones.contains_key(child) {
                        out.push(child.clone());
                    }
                    queue.push(child.clone());
                }
            }
        }
        Ok(out)
    }

    /// All registered agents with computed liveness, online first.
    pub fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>> {
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let now = Local::now();
        let tombstones = self.read_deleted_sessions_unlocked()?;
        let mut out: Vec<AgentInfo> = self
            .read_registry_unlocked()?
            .into_values()
            .filter(|e| !tombstones.contains_key(&e.session_id))
            .map(|e| AgentInfo {
                status: if is_online(&e.last_seen, now) {
                    AgentStatus::Online
                } else {
                    AgentStatus::Offline
                },
                session_id: e.session_id,
                last_seen: e.last_seen,
                started_at: e.started_at,
                title: e.title,
                role: e.role,
                parent_session_id: e.parent_session_id,
                capabilities: e.capabilities,
            })
            .collect();
        out.sort_by(|a, b| {
            let ka = (a.status == AgentStatus::Offline, &a.session_id);
            let kb = (b.status == AgentStatus::Offline, &b.session_id);
            ka.cmp(&kb)
        });
        Ok(out)
    }

    /// Count all child session identities, including tombstones, to preserve
    /// the historical cumulative spawn-cap contract.
    pub fn count_registered_children(&self, parent_session_id: &str) -> anyhow::Result<usize> {
        validate_session_id(parent_session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let reg = self.read_registry_unlocked()?;
        let tombstones = self.read_deleted_sessions_unlocked()?;
        let mut children = HashSet::new();
        for entry in reg.values() {
            if entry.parent_session_id.as_deref() == Some(parent_session_id) {
                children.insert(entry.session_id.as_str());
            }
        }
        for (session_id, entry) in &tombstones {
            if entry.parent_session_id.as_deref() == Some(parent_session_id) {
                children.insert(session_id.as_str());
            }
        }
        Ok(children.len())
    }

    /// Permanently tombstone session identities and remove their unacknowledged
    /// messages. The tombstone is committed before file cleanup, so a failed or
    /// interrupted cleanup remains fail-closed and can be retried idempotently.
    pub fn delete_sessions(&self, session_ids: &[String]) -> anyhow::Result<()> {
        let unique_ids = Self::unique_session_ids(session_ids)?;
        if unique_ids.is_empty() {
            return Ok(());
        }

        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        self.delete_sessions_unlocked(&unique_ids)
    }

    /// Tombstone the database's session identities plus any legacy identity
    /// still represented by files or registry metadata in the shared inbox.
    pub fn delete_all_sessions(&self, known_session_ids: &[String]) -> anyhow::Result<()> {
        let mut unique_ids = Self::unique_session_ids(known_session_ids)?;
        let mut seen: HashSet<String> = unique_ids.iter().cloned().collect();
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        for session_id in self
            .read_registry_unlocked()?
            .into_keys()
            .chain(self.read_deleted_sessions_unlocked()?.into_keys())
        {
            if seen.insert(session_id.clone()) {
                unique_ids.push(session_id);
            }
        }
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Some(session_id) = session_id_from_inbox_file_name(&name)
                && seen.insert(session_id.clone())
            {
                unique_ids.push(session_id);
            }
        }
        self.delete_sessions_unlocked(&unique_ids)
    }

    fn unique_session_ids(session_ids: &[String]) -> anyhow::Result<Vec<String>> {
        let mut unique_ids = Vec::with_capacity(session_ids.len());
        let mut seen = HashSet::with_capacity(session_ids.len());
        for session_id in session_ids {
            validate_session_id(session_id)?;
            if seen.insert(session_id.as_str()) {
                unique_ids.push(session_id.clone());
            }
        }
        Ok(unique_ids)
    }

    fn delete_sessions_unlocked(&self, session_ids: &[String]) -> anyhow::Result<()> {
        if session_ids.is_empty() {
            return Ok(());
        }
        let mut tombstones = self.read_deleted_sessions_unlocked()?;
        let mut registry = self.read_registry_unlocked()?;
        for session_id in session_ids {
            let parent_session_id = match tombstones.get(session_id) {
                Some(entry) => entry.parent_session_id.clone(),
                None => registry
                    .get(session_id)
                    .and_then(|entry| entry.parent_session_id.clone()),
            };
            tombstones.insert(
                session_id.clone(),
                DeletedSessionEntry { parent_session_id },
            );
        }
        self.write_deleted_sessions_unlocked(&tombstones)?;

        let mut registry_changed = false;
        for session_id in session_ids {
            registry_changed |= registry.remove(session_id).is_some();
        }
        if registry_changed {
            self.write_registry_unlocked(&registry)?;
        }

        for session_id in session_ids {
            self.delete_session_messages_unlocked(session_id)?;
        }
        Ok(())
    }

    /// Append one envelope to the recipient's mailbox. Lenient delivery: an
    /// existing mailbox still accepts a message when the recipient is
    /// unregistered or its heartbeat is stale (freshness is reported in
    /// [`SendOutcome::status`]); a missing mailbox or deleted-session
    /// tombstone is an error.
    pub fn deliver(&self, to: &str, env: &Envelope) -> anyhow::Result<SendOutcome> {
        validate_message_envelope_for_delivery(to, env)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let tombstones = self.read_deleted_sessions_unlocked()?;
        if tombstones.contains_key(to) {
            anyhow::bail!("cannot deliver to deleted session '{to}'")
        }
        if tombstones.contains_key(&env.from_session_id) {
            anyhow::bail!(
                "cannot deliver from deleted session '{}'",
                env.from_session_id
            )
        }
        self.recover_mailbox_tmp_unlocked(to)?;
        self.repair_mailbox_tail_unlocked(to)?;
        self.recover_processing_tmp_unlocked(to)?;
        self.recover_backlog_state_unlocked(to)?;
        let mailbox = self.mailbox(to);
        if !mailbox.exists() {
            anyhow::bail!("agent '{to}' not found or offline: no mailbox (never registered)")
        }
        let reg = self.read_registry_unlocked()?;
        let status = match reg.get(to) {
            Some(e) if is_online(&e.last_seen, Local::now()) => AgentStatus::Online,
            _ => AgentStatus::Offline,
        };
        let mut line = serde_json::to_string(env)?;
        line.push('\n');
        let pending_bytes = self.pending_bytes_unlocked(to)?;
        let incoming_bytes = line.len() as u64;
        if pending_bytes.saturating_add(incoming_bytes) > MAX_PENDING_BYTES {
            anyhow::bail!(
                "recipient inbox capacity exceeded: pending {pending_bytes} bytes, incoming {incoming_bytes} bytes, maximum {MAX_PENDING_BYTES} bytes"
            )
        }
        let mut f = OpenOptions::new().append(true).read(true).open(&mailbox)?;
        let original_len = f.metadata()?.len();
        if let Err(write_error) = f.write_all(line.as_bytes()).and_then(|()| f.flush()) {
            let rollback = f.set_len(original_len).and_then(|()| f.flush());
            if let Err(rollback_error) = rollback {
                anyhow::bail!(
                    "message append failed ({write_error}); mailbox rollback also failed ({rollback_error})"
                )
            }
            return Err(write_error.into());
        }
        // In-process wake-up: the recipient's ReAct loop can react to the
        // notification instead of waiting for its next polling step.
        self.notifier.notify();
        Ok(SendOutcome {
            to_session_id: to.into(),
            delivered: true,
            status,
        })
    }

    /// Claim mailbox messages for a durable consumer without acknowledging
    /// them yet.
    ///
    /// This leaves the claimed bounded batch in `<session_id>.jsonl.processing`. The
    /// service must call the acknowledgement primitive
    /// [`Self::ack_claimed`]
    /// after it has durably projected the batch. A crash between claim and ack
    /// therefore produces at-least-once delivery instead of silently losing a
    /// message after it was archived.
    pub(crate) fn claim_and_archive(&self, session_id: &str) -> anyhow::Result<Vec<Envelope>> {
        validate_session_id(session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.claim_and_archive_unlocked(session_id)
    }

    /// Best-effort claim for heartbeat/background polling. It never waits for
    /// the shared inbox lock; a concurrent sender or explicit messaging
    /// operation owns the lock, so the caller can retry on its next cadence.
    pub(crate) fn try_claim_and_archive(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<Vec<Envelope>>> {
        validate_session_id(session_id)?;
        let Some(_lock) = LockGuard::try_acquire(&self.root)? else {
            return Ok(None);
        };
        self.claim_and_archive_unlocked(session_id).map(Some)
    }

    fn claim_and_archive_unlocked(&self, session_id: &str) -> anyhow::Result<Vec<Envelope>> {
        self.ensure_dir()?;
        self.ensure_session_not_deleted_unlocked(session_id, "claim messages for")?;
        self.recover_mailbox_tmp_unlocked(session_id)?;
        self.repair_mailbox_tail_unlocked(session_id)?;
        self.recover_processing_tmp_unlocked(session_id)?;
        self.recover_backlog_state_unlocked(session_id)?;
        // Recover an archive replacement left between Windows' remove and
        // rename steps before processing the current claim.
        self.append_archive_unlocked(session_id, &[])?;
        let Some(mut state) = self.ensure_backlog_ready_unlocked(session_id)? else {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.mailbox(session_id))?;
            return Ok(Vec::new());
        };

        if let Some(active_end) = state.active_end_bytes {
            let source = self.backlog(session_id);
            let envs = read_backlog_range(session_id, &source, state.cursor_bytes, active_end)?;
            let active = match self.read_processing_unlocked(session_id) {
                Ok(mut active) if !active.is_empty() => {
                    state.active_attempts =
                        state.active_attempts.max(inferred_attempts(&envs, &active));
                    for envelope in &mut active {
                        envelope.delivery_attempt = envelope.delivery_attempt.saturating_add(1);
                    }
                    state.active_attempts = state.active_attempts.saturating_add(1);
                    active
                }
                Ok(_) | Err(_) => {
                    if state.active_attempts == 0 {
                        let archive_ids = self.read_archive_tail_ids(session_id)?;
                        let active = select_active_envelopes(&envs, &archive_ids);
                        if !active.is_empty() {
                            state.active_attempts = 1;
                        }
                        active
                    } else {
                        select_at_attempts(&envs, state.active_attempts)
                    }
                }
            };
            self.write_processing_unlocked(session_id, &active)?;
            self.write_backlog_state_unlocked(session_id, &state)?;
            // Archive from the immutable source range, so retries after an
            // archive failure can reproduce the same records and dedupe IDs.
            self.append_archive_unlocked(session_id, &envs)?;
            if active.is_empty() {
                self.advance_backlog_cursor_unlocked(session_id, &mut state, active_end)?;
                return Ok(Vec::new());
            }
            return Ok(active);
        }

        let source = self.backlog(session_id);
        let Some((end_bytes, envs)) = read_backlog_batch(session_id, &source, state.cursor_bytes)?
        else {
            state.phase = BacklogPhase::Complete;
            self.write_backlog_state_unlocked(session_id, &state)?;
            self.recover_backlog_state_unlocked(session_id)?;
            return Ok(Vec::new());
        };

        state.active_end_bytes = Some(end_bytes);
        state.active_attempts = 0;
        self.write_backlog_state_unlocked(session_id, &state)?;
        let archive_ids = self.read_archive_tail_ids(session_id)?;
        let active = select_active_envelopes(&envs, &archive_ids);
        self.write_processing_unlocked(session_id, &active)?;
        state.active_attempts = if active.is_empty() { 0 } else { 1 };
        self.write_backlog_state_unlocked(session_id, &state)?;
        self.append_archive_unlocked(session_id, &envs)?;
        if active.is_empty() {
            self.advance_backlog_cursor_unlocked(session_id, &mut state, end_bytes)?;
            return Ok(Vec::new());
        }
        Ok(active)
    }

    /// Acknowledge a previously claimed batch. Only matching ids are removed;
    /// messages that arrived after the claim remain in the append mailbox and
    /// are promoted after the older immutable backlog has drained.
    pub(crate) fn ack_claimed(&self, session_id: &str, ids: &[String]) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        for id in ids {
            validate_message_id(id, "message_id")?;
        }
        if ids.is_empty() {
            return Ok(());
        }
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        self.ensure_session_not_deleted_unlocked(session_id, "acknowledge messages for")?;
        self.recover_mailbox_tmp_unlocked(session_id)?;
        self.recover_archive_tmp_unlocked(session_id)?;
        self.recover_processing_tmp_unlocked(session_id)?;
        self.recover_backlog_state_unlocked(session_id)?;
        let Some(mut state) = self.ensure_backlog_ready_unlocked(session_id)? else {
            return Ok(());
        };
        let Some(active_end) = state.active_end_bytes else {
            return Ok(());
        };
        let source_envelopes = read_backlog_range(
            session_id,
            &self.backlog(session_id),
            state.cursor_bytes,
            active_end,
        )?;
        let processing = match self.read_processing_unlocked(session_id) {
            Ok(processing) if !processing.is_empty() => processing,
            Ok(_) | Err(_) => {
                if state.active_attempts == 0 {
                    let archive_ids = self.read_archive_tail_ids(session_id)?;
                    select_active_envelopes(&source_envelopes, &archive_ids)
                } else {
                    select_at_attempts(&source_envelopes, state.active_attempts)
                }
            }
        };
        state.active_attempts = state
            .active_attempts
            .max(inferred_attempts(&source_envelopes, &processing));
        if state.active_attempts == 0 && !processing.is_empty() {
            state.active_attempts = 1;
        }
        self.write_processing_unlocked(session_id, &processing)?;
        self.write_backlog_state_unlocked(session_id, &state)?;
        let acknowledged: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let remaining_envelopes: Vec<Envelope> = processing
            .into_iter()
            .filter(|env| !acknowledged.contains(env.message_id.as_str()))
            .collect();
        self.write_processing_unlocked(session_id, &remaining_envelopes)?;
        if remaining_envelopes.is_empty() {
            self.advance_backlog_cursor_unlocked(session_id, &mut state, active_end)?;
        }
        Ok(())
    }

    /// The most recent message this agent received (unread mailbox first,
    /// then archive tail). Used by the service to resolve a missing reply
    /// target.
    pub fn last_received(&self, session_id: &str) -> anyhow::Result<Option<Envelope>> {
        validate_session_id(session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        self.ensure_session_not_deleted_unlocked(session_id, "query messages for")?;
        self.recover_mailbox_tmp_unlocked(session_id)?;
        self.repair_mailbox_tail_unlocked(session_id)?;
        self.recover_backlog_state_unlocked(session_id)?;
        self.append_archive_unlocked(session_id, &[])?;
        if let Some(env) = last_valid_line(session_id, &self.mailbox(session_id))? {
            return Ok(Some(env));
        }
        if let Some(state) = self.read_backlog_state_unlocked(session_id)? {
            if state.phase == BacklogPhase::Ready
                && let Some(env) =
                    last_valid_line_from(session_id, &self.backlog(session_id), state.cursor_bytes)?
            {
                return Ok(Some(env));
            }
        } else if let Some(env) = last_valid_line(session_id, &self.processing(session_id))? {
            return Ok(Some(env));
        }
        last_valid_line(session_id, &self.archive(session_id))
    }

    /// Find one envelope by id in this agent's mailbox or archive. Used by
    /// the service to resolve the target of an `in_reply_to_message_id` reference.
    /// The archive is scanned only within its bounded durable tail; very old
    /// records are intentionally outside the retention contract.
    pub fn find_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<Envelope>> {
        validate_session_id(session_id)?;
        validate_message_id(message_id, "message_id")?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        self.ensure_session_not_deleted_unlocked(session_id, "query messages for")?;
        self.recover_mailbox_tmp_unlocked(session_id)?;
        self.repair_mailbox_tail_unlocked(session_id)?;
        self.recover_backlog_state_unlocked(session_id)?;
        self.append_archive_unlocked(session_id, &[])?;
        let mut sources = vec![(self.mailbox(session_id), 0)];
        if let Some(state) = self.read_backlog_state_unlocked(session_id)? {
            if state.phase == BacklogPhase::Ready {
                sources.push((self.backlog(session_id), state.cursor_bytes));
            }
        } else {
            sources.push((self.processing(session_id), 0));
        }
        for (path, start_bytes) in sources {
            if let Some(envelope) = find_in_jsonl_bounded(
                session_id,
                &path,
                start_bytes,
                MAX_PENDING_BYTES,
                message_id,
            )? {
                return Ok(Some(envelope));
            }
        }
        for line in read_tail(&self.archive(session_id), MAX_ARCHIVE_BYTES)?.lines() {
            if let Some(envelope) = parse_envelope(session_id, line.trim())
                .ok()
                .filter(|envelope| envelope.message_id == message_id)
            {
                return Ok(Some(envelope));
            }
        }
        Ok(None)
    }

    /// Selectively drain mailbox envelopes that reply to `in_reply_to_message_id` from
    /// `expected_from_session_id`, leaving unrelated messages in place. Used by the
    /// service request wait so a blocked RPC does not steal peer mail meant
    /// for the ReAct auto-inject path, and so a third agent cannot satisfy the
    /// wait by forging `in_reply_to_message_id`.
    ///
    /// Matching rule: `in_reply_to_message_id` equals the request id, `from_session_id` equals the
    /// original recipient, and type is `reply` or `message` (not `receipt`).
    /// Expired matches are archived but not returned.
    pub(crate) fn take_matching_replies(
        &self,
        session_id: &str,
        in_reply_to_message_id: &str,
        expected_from_session_id: &str,
    ) -> anyhow::Result<Vec<Envelope>> {
        validate_session_id(session_id)?;
        validate_session_id(expected_from_session_id)?;
        validate_message_id(in_reply_to_message_id, "in_reply_to_message_id")?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        self.ensure_session_not_deleted_unlocked(session_id, "read replies for")?;
        self.recover_mailbox_tmp_unlocked(session_id)?;
        self.repair_mailbox_tail_unlocked(session_id)?;
        self.append_archive_unlocked(session_id, &[])?;
        let mailbox = self.mailbox(session_id);
        let input = match File::open(&mailbox) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let tmp = self.mailbox_tmp(session_id);
        let mut output = File::create(&tmp)?;
        let mut reader = BufReader::new(input);
        let mut scanned = 0u64;
        let mut matching: Vec<Envelope> = Vec::new();
        let mut archived_only: Vec<Envelope> = Vec::new();
        loop {
            let Some(line) = read_bounded_line(&mut reader)? else {
                break;
            };
            scanned = scanned.saturating_add(line.len() as u64);
            if scanned > MAX_PENDING_BYTES {
                anyhow::bail!(
                    "mailbox exceeds bounded reply-scan budget of {MAX_PENDING_BYTES} bytes; mailbox is preserved"
                )
            }
            let parsed = std::str::from_utf8(&line)
                .ok()
                .and_then(|line| parse_envelope(session_id, line.trim()).ok());
            match parsed {
                Some(env)
                    if is_reply_to_request(
                        &env,
                        in_reply_to_message_id,
                        expected_from_session_id,
                    ) && matching.len() + archived_only.len() < MAX_CLAIM_BATCH_LINES =>
                {
                    if is_expired(&env) {
                        archived_only.push(env);
                    } else {
                        matching.push(env);
                    }
                }
                _ => output.write_all(&line)?,
            }
        }

        if matching.is_empty() && archived_only.is_empty() {
            remove_if_exists(&tmp)?;
            return Ok(Vec::new());
        }

        // Archive replies before removing them from the mailbox. If archive
        // persistence fails, the original mailbox remains retryable.
        let to_archive: Vec<Envelope> = matching
            .iter()
            .chain(archived_only.iter())
            .cloned()
            .collect();
        self.append_archive_unlocked(session_id, &to_archive)?;
        output.flush()?;
        output.sync_all()?;
        self.commit_mailbox_tmp_unlocked(session_id)?;
        Ok(matching)
    }

    /// Message history of one agent: mailbox messages plus the bounded
    /// delivery archive, newest first, up to `limit` entries. The archive is
    /// appended before acknowledgement and is not a read-status marker. Read-only view for
    /// the UI / audit (never consumes the mailbox).
    pub fn history(&self, session_id: &str, limit: usize) -> anyhow::Result<Vec<Envelope>> {
        validate_session_id(session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        self.ensure_session_not_deleted_unlocked(session_id, "query history for")?;
        self.recover_mailbox_tmp_unlocked(session_id)?;
        self.repair_mailbox_tail_unlocked(session_id)?;
        self.recover_processing_tmp_unlocked(session_id)?;
        self.recover_backlog_state_unlocked(session_id)?;
        self.append_archive_unlocked(session_id, &[])?;
        let limit = limit.min(MAX_MESSAGE_HISTORY_LIMIT);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        let mut positions = HashMap::new();
        let archive = read_tail(&self.archive(session_id), MAX_ARCHIVE_BYTES)?;
        append_history_source(session_id, &archive, &mut entries, &mut positions)?;
        if let Some(state) = self.read_backlog_state_unlocked(session_id)? {
            if state.phase == BacklogPhase::Ready {
                let backlog = read_tail_from(
                    &self.backlog(session_id),
                    state.cursor_bytes,
                    MAX_PENDING_BYTES,
                )?;
                append_history_source(session_id, &backlog, &mut entries, &mut positions)?;
            }
        } else {
            let processing = read_tail(&self.processing(session_id), MAX_PENDING_BYTES)?;
            append_history_source(session_id, &processing, &mut entries, &mut positions)?;
        }
        let mailbox = read_tail(&self.mailbox(session_id), MAX_PENDING_BYTES)?;
        append_history_source(session_id, &mailbox, &mut entries, &mut positions)?;
        entries.reverse();
        entries.truncate(limit);
        Ok(entries)
    }

    fn prepare_recovery_state_unlocked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<BacklogState>> {
        self.ensure_dir()?;
        self.ensure_session_not_deleted_unlocked(session_id, "recover messages for")?;
        self.recover_mailbox_tmp_unlocked(session_id)?;
        self.recover_processing_tmp_unlocked(session_id)?;
        self.recover_backlog_state_unlocked(session_id)?;
        self.ensure_backlog_ready_unlocked(session_id)
    }

    fn recovery_target_unlocked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<(BacklogState, OversizedInboxRow)>> {
        let Some(state) = self.prepare_recovery_state_unlocked(session_id)? else {
            return Ok(None);
        };
        if state.active_end_bytes.is_some() || file_len_or_zero(&self.processing(session_id))? > 0 {
            anyhow::bail!(
                "the inbox has an unacknowledged bounded batch; finish its normal delivery and acknowledgement before quarantining the next row"
            )
        }
        let Some(row) = inspect_oversized_row(&self.backlog(session_id), state.cursor_bytes)?
        else {
            return Ok(None);
        };
        Ok(Some((state, row)))
    }

    /// Inspect only the next row at the durable cursor, which is the row that
    /// blocks forward progress. It does not parse or return message content.
    fn inspect_next_oversized_row(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<OversizedInboxRow>> {
        validate_session_id(session_id)?;
        let _lock = LockGuard::acquire(&self.root)?;
        Ok(self
            .recovery_target_unlocked(session_id)?
            .map(|(_, row)| row))
    }

    /// Copy the exact oversized row to a bounded evidence file, then advance
    /// the durable cursor. The expected offset and digest make stale or
    /// accidental targets fail closed. Repeating the same request after a
    /// crash is idempotent.
    fn quarantine_next_oversized_row(
        &self,
        session_id: &str,
        expected_offset_bytes: u64,
        expected_sha256: &str,
    ) -> anyhow::Result<QuarantinedInboxRow> {
        validate_session_id(session_id)?;
        validate_sha256(expected_sha256)?;
        let _lock = LockGuard::acquire(&self.root)?;
        let existing =
            self.existing_quarantine_row(session_id, expected_offset_bytes, expected_sha256)?;
        let Some(mut state) = self.prepare_recovery_state_unlocked(session_id)? else {
            return existing.ok_or_else(|| {
                anyhow::anyhow!("there is no oversized row at the current inbox cursor")
            });
        };
        if state.cursor_bytes > expected_offset_bytes {
            return existing.ok_or_else(|| {
                anyhow::anyhow!(
                    "the inbox cursor has moved past this row but its quarantine evidence is missing"
                )
            });
        }
        if state.cursor_bytes < expected_offset_bytes {
            anyhow::bail!(
                "the inbox cursor has not reached this row; inspect the current cursor before retrying"
            )
        }
        if state.active_end_bytes.is_some() || file_len_or_zero(&self.processing(session_id))? > 0 {
            anyhow::bail!(
                "the inbox has an unacknowledged bounded batch; finish its normal delivery and acknowledgement before quarantining the next row"
            )
        }
        let row = inspect_oversized_row(&self.backlog(session_id), state.cursor_bytes)?
            .ok_or_else(|| {
                anyhow::anyhow!("there is no oversized row at the current inbox cursor")
            })?;
        if row.offset_bytes != expected_offset_bytes || row.sha256 != expected_sha256 {
            anyhow::bail!(
                "the inbox row changed since inspection; inspect the current cursor again before retrying"
            )
        }
        let quarantine_file = self.quarantine_row(session_id, &row);
        self.persist_quarantine_row(session_id, &row, &quarantine_file)?;
        self.advance_backlog_cursor_unlocked(
            session_id,
            &mut state,
            row.offset_bytes.saturating_add(row.row_bytes),
        )?;
        Ok(QuarantinedInboxRow {
            row,
            quarantine_file,
        })
    }

    fn existing_quarantine_row(
        &self,
        session_id: &str,
        offset_bytes: u64,
        sha256: &str,
    ) -> anyhow::Result<Option<QuarantinedInboxRow>> {
        let row = OversizedInboxRow {
            offset_bytes,
            row_bytes: 0,
            sha256: sha256.to_string(),
        };
        let quarantine_file = self.quarantine_row(session_id, &row);
        let metadata = match std::fs::symlink_metadata(&quarantine_file) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => anyhow::bail!("inbox quarantine evidence is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let row = OversizedInboxRow {
            row_bytes: metadata.len(),
            ..row
        };
        if row.row_bytes <= MAX_ENVELOPE_LINE_BYTES as u64 {
            anyhow::bail!("existing inbox quarantine evidence has an invalid byte length")
        }
        verify_file_digest(&quarantine_file, row.row_bytes, sha256)?;
        Ok(Some(QuarantinedInboxRow {
            row,
            quarantine_file,
        }))
    }

    fn persist_quarantine_row(
        &self,
        session_id: &str,
        row: &OversizedInboxRow,
        quarantine_file: &Path,
    ) -> anyhow::Result<()> {
        if quarantine_file.exists() {
            verify_file_digest(quarantine_file, row.row_bytes, &row.sha256)?;
            return Ok(());
        }
        let tmp = self.quarantine_row_tmp(session_id, row);
        remove_if_exists(&tmp)?;
        self.remove_quarantine_temps_unlocked()?;
        let current_bytes = quarantine_bytes_in_root(&self.root)?;
        if current_bytes.saturating_add(row.row_bytes) > MAX_QUARANTINE_BYTES {
            anyhow::bail!(
                "Messaging quarantine evidence would exceed its {MAX_QUARANTINE_BYTES}-byte inbox budget; back up and reset the inbox to recover this row"
            )
        }
        let source = self.backlog(session_id);
        let mut input = File::open(source)?;
        input.seek(SeekFrom::Start(row.offset_bytes))?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let copied_digest = match copy_row_bounded(
            &mut input,
            &mut output,
            row.row_bytes,
            MAX_QUARANTINED_ROW_BYTES,
        ) {
            Ok(digest) => digest,
            Err(error) => {
                drop(output);
                remove_if_exists(&tmp)?;
                return Err(error);
            }
        };
        if copied_digest != row.sha256 {
            drop(output);
            remove_if_exists(&tmp)?;
            anyhow::bail!(
                "the source row changed while it was being quarantined; the backlog cursor was not advanced"
            )
        }
        output.flush()?;
        output.sync_all()?;
        drop(output);
        std::fs::rename(&tmp, quarantine_file)?;
        Ok(())
    }

    fn remove_quarantine_temps_unlocked(&self) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if quarantine_file_parts(&name).is_some_and(|(_, _, _, is_tmp)| is_tmp) {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    fn read_registry_unlocked(&self) -> anyhow::Result<HashMap<String, AgentEntry>> {
        let path = self.root.join("agents.json");
        match std::fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => Ok(HashMap::new()),
            Ok(s) => {
                let registry: HashMap<String, AgentEntry> = serde_json::from_str(&s)
                    .map_err(|e| anyhow::anyhow!("corrupt agents.json: {e}"))?;
                for (session_id, entry) in &registry {
                    validate_session_id(session_id).map_err(|error| {
                        anyhow::anyhow!("invalid session id in agents.json: {error}")
                    })?;
                    if entry.session_id != *session_id {
                        anyhow::bail!(
                            "corrupt agents.json: key '{session_id}' does not match entry session_id '{}'",
                            entry.session_id
                        );
                    }
                    if let Some(parent_session_id) = entry.parent_session_id.as_deref() {
                        validate_session_id(parent_session_id).map_err(|error| {
                            anyhow::anyhow!("invalid parent session ID in agents.json: {error}")
                        })?;
                    }
                }
                Ok(registry)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn is_deleted_session_unlocked(&self, session_id: &str) -> anyhow::Result<bool> {
        Ok(self
            .read_deleted_sessions_unlocked()?
            .contains_key(session_id))
    }

    fn ensure_session_not_deleted_unlocked(
        &self,
        session_id: &str,
        operation: &str,
    ) -> anyhow::Result<()> {
        if self.is_deleted_session_unlocked(session_id)? {
            anyhow::bail!("cannot {operation} deleted session '{session_id}'")
        }
        Ok(())
    }

    fn read_deleted_sessions_unlocked(
        &self,
    ) -> anyhow::Result<HashMap<String, DeletedSessionEntry>> {
        self.recover_deleted_sessions_tmp_unlocked()?;
        let path = self.deleted_sessions();
        match read_bounded_text(&path, MAX_DELETED_SESSIONS_BYTES) {
            Ok(contents) if contents.trim().is_empty() => Ok(HashMap::new()),
            Ok(contents) => parse_deleted_sessions(&contents),
            Err(error) if is_not_found(&error) => Ok(HashMap::new()),
            Err(error) => Err(error),
        }
    }

    fn write_deleted_sessions_unlocked(
        &self,
        deleted: &HashMap<String, DeletedSessionEntry>,
    ) -> anyhow::Result<()> {
        for (session_id, entry) in deleted {
            validate_session_id(session_id)?;
            if let Some(parent_session_id) = entry.parent_session_id.as_deref() {
                validate_session_id(parent_session_id)?;
            }
        }
        let path = self.deleted_sessions();
        let tmp = self.deleted_sessions_tmp();
        let backup = self.deleted_sessions_backup();
        let contents = serde_json::to_vec_pretty(deleted)?;
        validate_deleted_sessions_size(contents.len())?;
        let mut file = File::create(&tmp)?;
        file.write_all(&contents)?;
        file.flush()?;
        file.sync_all()?;

        // Keep the prior committed lineage recoverable until the new complete
        // file is installed. A crash after moving the canonical file must not
        // turn a damaged temp into an empty tombstone set.
        remove_if_exists(&backup)?;
        if path.exists() {
            std::fs::rename(&path, &backup)?;
        }
        std::fs::rename(&tmp, &path)?;
        remove_if_exists(&backup)?;
        Ok(())
    }

    fn delete_session_messages_unlocked(&self, session_id: &str) -> anyhow::Result<()> {
        let message_paths = [
            self.mailbox(session_id),
            self.mailbox_tmp(session_id),
            self.processing(session_id),
            self.processing_tmp(session_id),
            self.backlog(session_id),
            self.backlog_state(session_id),
            self.backlog_state_tmp(session_id),
            self.archive(session_id),
            self.archive_tmp(session_id),
        ];
        for path in message_paths {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for path in self.quarantine_rows_for_session(session_id)? {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn quarantine_rows_for_session(&self, session_id: &str) -> anyhow::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if quarantine_file_parts(&name)
                .is_some_and(|(candidate, _, _, _)| candidate == session_id)
            {
                paths.push(entry.path());
            }
        }
        Ok(paths)
    }

    fn recover_deleted_sessions_tmp_unlocked(&self) -> anyhow::Result<()> {
        let path = self.deleted_sessions();
        let tmp = self.deleted_sessions_tmp();
        let backup = self.deleted_sessions_backup();
        if path.exists() {
            // The canonical file is authoritative when present. Validate it
            // before removing recovery files so corruption remains fail-closed.
            parse_deleted_sessions_file(&path)?;
            remove_if_exists(&tmp)?;
            remove_if_exists(&backup)?;
            return Ok(());
        }

        if tmp.exists() {
            match parse_deleted_sessions_file(&tmp) {
                Ok(_) => {
                    std::fs::rename(&tmp, &path)?;
                    remove_if_exists(&backup)?;
                    return Ok(());
                }
                Err(error) if is_transient_file_read_error(&error) => return Err(error),
                Err(temp_error) if backup.exists() => {
                    // A prior committed map may have been moved aside before
                    // an interrupted replacement. Restore it before allowing
                    // reads or writes to proceed.
                    parse_deleted_sessions_file(&backup).map_err(|backup_error| {
                        anyhow::anyhow!(
                            "invalid deleted-session temp and backup; temp: {temp_error:#}; backup: {backup_error:#}"
                        )
                    })?;
                    std::fs::rename(&backup, &path)?;
                    remove_if_exists(&tmp)?;
                    return Ok(());
                }
                // With no prior canonical map or backup, an invalid first
                // write was never committed and can be retried safely.
                Err(_) => remove_if_exists(&tmp)?,
            }
        }

        if backup.exists() {
            parse_deleted_sessions_file(&backup)?;
            std::fs::rename(&backup, &path)?;
        }
        Ok(())
    }

    /// Append envelopes to the archive and compact it to a newest-record tail
    /// in the same locked mutation. Read-only callers do not compact as a
    /// side effect; interrupted replacements are still recovered.
    fn append_archive_unlocked(
        &self,
        session_id: &str,
        envelopes: &[Envelope],
    ) -> anyhow::Result<()> {
        #[cfg(test)]
        if !envelopes.is_empty()
            && self
                .fail_archive_append
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("injected archive append failure");
        }

        // Read-only callers enter this helper too, so do not turn every
        // history/find lookup into a remove/create/sync cycle. A temp file is
        // itself a recovery signal; otherwise a rewrite is needed only when
        // the archive is oversized or at least one envelope is genuinely new.
        let had_tmp = self.recover_archive_tmp_unlocked(session_id)?;
        let archive = self.archive(session_id);
        let existing = read_tail(&archive, MAX_ARCHIVE_BYTES)?;
        let mut lines: Vec<String> = existing
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        let mut ids: HashSet<String> = lines
            .iter()
            .filter_map(|line| parse_envelope(session_id, line).ok())
            .map(|envelope| envelope.message_id)
            .collect();
        let mut has_new_envelope = false;
        for envelope in envelopes {
            if ids.insert(envelope.message_id.clone()) {
                has_new_envelope = true;
                lines.push(serde_json::to_string(envelope)?);
            }
        }

        let mut retained = Vec::new();
        let mut bytes = 0u64;
        for line in lines.into_iter().rev() {
            let line_bytes = line.len() as u64 + 1;
            if line_bytes > MAX_ARCHIVE_BYTES {
                tracing::warn!(
                    session_id = session_id,
                    bytes = line_bytes,
                    "inbox archive record exceeds archive byte budget; dropping record from audit tail"
                );
                continue;
            }
            if bytes.saturating_add(line_bytes) > MAX_ARCHIVE_BYTES {
                break;
            }
            bytes += line_bytes;
            retained.push(line);
        }
        retained.reverse();

        if !had_tmp && !has_new_envelope {
            return Ok(());
        }

        let tmp = self.archive_tmp(session_id);
        let mut file = File::create(&tmp)?;
        for line in retained {
            writeln!(file, "{line}")?;
        }
        file.flush()?;
        file.sync_all()?;
        match std::fs::remove_file(&archive) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        std::fs::rename(tmp, archive)?;
        Ok(())
    }

    /// Recover the archive replacement after an interrupted Windows
    /// remove-then-rename. A temp file is promoted only when the destination
    /// is absent; if the old archive survived, it remains the safer source of
    /// truth and the abandoned temp is discarded.
    fn recover_archive_tmp_unlocked(&self, session_id: &str) -> anyhow::Result<bool> {
        let archive = self.archive(session_id);
        let tmp = self.archive_tmp(session_id);
        if !tmp.exists() {
            return Ok(false);
        }
        if archive.exists() {
            std::fs::remove_file(tmp)?;
        } else {
            std::fs::rename(tmp, archive)?;
        }
        Ok(true)
    }

    /// Atomic registry update: write a temp file, then rename over
    /// `agents.json` (caller must hold the lock).
    fn write_registry_unlocked(&self, reg: &HashMap<String, AgentEntry>) -> anyhow::Result<()> {
        for (session_id, entry) in reg {
            validate_session_id(session_id)?;
            if entry.session_id != *session_id {
                anyhow::bail!(
                    "registry key '{session_id}' does not match entry session_id '{}'",
                    entry.session_id
                );
            }
            if let Some(parent_session_id) = entry.parent_session_id.as_deref() {
                validate_session_id(parent_session_id)?;
            }
        }
        let tmp = self.root.join("agents.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(reg)?)?;
        std::fs::rename(&tmp, self.root.join("agents.json"))?;
        Ok(())
    }

    /// Ids of the last messages in the archive, for append deduplication
    /// (crash recovery only; bounded scan keeps this O(1)-ish in practice).
    fn read_archive_tail_ids(&self, session_id: &str) -> anyhow::Result<HashSet<String>> {
        Ok(read_tail(&self.archive(session_id), MAX_ARCHIVE_BYTES)?
            .lines()
            .filter_map(|line| parse_envelope(session_id, line.trim()).ok())
            .map(|e| e.message_id)
            .collect())
    }

    /// Commit the fully written selective reply rewrite. The temp file is
    /// recoverable across Windows' remove-then-rename window.
    fn commit_mailbox_tmp_unlocked(&self, session_id: &str) -> anyhow::Result<()> {
        let mailbox = self.mailbox(session_id);
        let tmp = self.mailbox_tmp(session_id);
        match std::fs::remove_file(&mailbox) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        std::fs::rename(tmp, mailbox)?;
        Ok(())
    }

    fn recover_mailbox_tmp_unlocked(&self, session_id: &str) -> anyhow::Result<()> {
        let mailbox = self.mailbox(session_id);
        let tmp = self.mailbox_tmp(session_id);
        if !tmp.exists() {
            return Ok(());
        }

        if mailbox.exists() {
            // The old mailbox survived, so the temp file was not committed.
            // Keep the old source of truth, which still includes any replies.
            std::fs::remove_file(tmp)?;
        } else {
            // The old mailbox was removed before a crash; promote the fully
            // synced replacement. Archived replies remain durably recoverable.
            std::fs::rename(tmp, mailbox)?;
        }
        Ok(())
    }

    fn repair_mailbox_tail_unlocked(&self, session_id: &str) -> anyhow::Result<()> {
        let path = self.mailbox(session_id);
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let length = file.metadata()?.len();
        if length == 0 {
            return Ok(());
        }
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0; 1];
        file.read_exact(&mut last)?;
        if last[0] == b'\n' {
            return Ok(());
        }

        let start = length.saturating_sub(MAX_ENVELOPE_LINE_BYTES as u64);
        file.seek(SeekFrom::Start(start))?;
        let mut tail = Vec::with_capacity((length - start) as usize);
        file.read_to_end(&mut tail)?;
        let last_newline = tail.iter().rposition(|byte| *byte == b'\n');
        let line_start_in_tail = last_newline.map_or(0, |index| index + 1);
        if start > 0 && last_newline.is_none() {
            anyhow::bail!(
                "unterminated inbox JSONL row exceeds the bounded repair limit; the mailbox is preserved"
            )
        }
        let final_line = &tail[line_start_in_tail..];
        if final_line.len() > MAX_MESSAGE_ENVELOPE_BYTES {
            anyhow::bail!(
                "unterminated inbox JSONL row exceeds the bounded repair limit; the mailbox is preserved"
            )
        }
        let line = std::str::from_utf8(final_line)?;
        if parse_envelope(session_id, line.trim()).is_ok() {
            file.seek(SeekFrom::End(0))?;
            file.write_all(b"\n")?;
            file.flush()?;
        } else {
            let truncate_to = start + line_start_in_tail as u64;
            file.set_len(truncate_to)?;
            file.flush()?;
        }
        Ok(())
    }

    /// Rewrite a claimed processing file while the inbox lock is held. The
    /// contents are fully written before replacement, and an interrupted
    /// replacement is recovered from the temp file on the next claim. An
    /// empty claim is removed so it cannot be mistaken for a pending delivery
    /// on the next poll.
    fn write_processing_unlocked(
        &self,
        session_id: &str,
        envelopes: &[Envelope],
    ) -> anyhow::Result<()> {
        let pending = self.processing(session_id);
        if envelopes.is_empty() {
            match std::fs::remove_file(&pending) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            return Ok(());
        }

        let lines = envelopes
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
        let serialized_bytes = lines
            .iter()
            .map(|line| line.len().saturating_add(1))
            .sum::<usize>();
        if serialized_bytes > MAX_PROCESSING_BYTES {
            anyhow::bail!("processing batch exceeds its bounded byte budget")
        }
        let tmp = self.processing_tmp(session_id);
        let mut file = File::create(&tmp)?;
        for line in lines {
            writeln!(file, "{line}")?;
        }
        file.flush()?;
        file.sync_all()?;
        // Windows cannot rename over an existing destination. Remove-and-move
        // is still crash-safe here: if the process dies after removing the
        // old claim, the next claim/ack call restores this fully-written temp
        // file before touching the mailbox.
        match std::fs::remove_file(&pending) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        std::fs::rename(tmp, pending)?;
        Ok(())
    }

    fn recover_processing_tmp_unlocked(&self, session_id: &str) -> anyhow::Result<()> {
        let pending = self.processing(session_id);
        let tmp = self.processing_tmp(session_id);
        if !tmp.exists() {
            return Ok(());
        }

        if pending.exists() {
            // The old claim survived, so the temp file was written before the
            // replacement phase. The old claim is the recoverable source of
            // truth; discard only the abandoned temp file.
            std::fs::remove_file(tmp)?;
        } else {
            std::fs::rename(tmp, pending)?;
        }
        Ok(())
    }

    fn recover_backlog_state_unlocked(&self, session_id: &str) -> anyhow::Result<()> {
        let state_path = self.backlog_state(session_id);
        let state_tmp = self.backlog_state_tmp(session_id);
        if state_tmp.exists() {
            if state_path.exists() {
                std::fs::remove_file(state_tmp)?;
            } else {
                let candidate = read_bounded_text(&state_tmp, MAX_BACKLOG_STATE_BYTES)
                    .ok()
                    .and_then(|contents| serde_json::from_str::<BacklogState>(&contents).ok());
                if candidate.as_ref().is_some_and(|state| {
                    state
                        .active_end_bytes
                        .is_none_or(|end| end >= state.cursor_bytes)
                        && (state.active_end_bytes.is_some() || state.active_attempts == 0)
                }) {
                    std::fs::rename(state_tmp, &state_path)?;
                } else if self.backlog(session_id).exists() {
                    anyhow::bail!(
                        "inbox backlog state replacement is corrupt; backlog source is preserved"
                    )
                } else {
                    // State is written and synced before any source rename.
                    // With the old source still in place, an incomplete first
                    // temp file can be discarded and rebuilt safely.
                    std::fs::remove_file(state_tmp)?;
                }
            }
        }

        let mut state = match read_bounded_text(&state_path, MAX_BACKLOG_STATE_BYTES) {
            Ok(contents) => {
                let state: BacklogState = serde_json::from_str(&contents)
                    .map_err(|error| anyhow::anyhow!("corrupt inbox backlog state: {error}"))?;
                if state
                    .active_end_bytes
                    .is_some_and(|end| end < state.cursor_bytes)
                    || (state.active_end_bytes.is_none() && state.active_attempts != 0)
                {
                    anyhow::bail!("invalid inbox backlog state")
                }
                Some(state)
            }
            Err(error) if is_not_found(&error) => None,
            Err(error) => return Err(error),
        };

        let Some(current) = state.as_mut() else {
            if self.backlog(session_id).exists() {
                anyhow::bail!("inbox backlog exists without its cursor state")
            }
            return Ok(());
        };

        match current.phase {
            BacklogPhase::Preparing => {
                let backlog = self.backlog(session_id);
                if !backlog.exists() {
                    let source = match current.source {
                        BacklogSource::Mailbox => self.mailbox(session_id),
                        BacklogSource::Processing => self.processing(session_id),
                    };
                    if !source.exists() {
                        anyhow::bail!("inbox backlog preparation lost its source file")
                    }
                    std::fs::rename(source, backlog)?;
                }
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.mailbox(session_id))?;
                current.phase = BacklogPhase::Ready;
                self.write_backlog_state_unlocked(session_id, current)?;
            }
            BacklogPhase::Ready => {
                let length = std::fs::metadata(self.backlog(session_id))?.len();
                if current.cursor_bytes > length
                    || current.active_end_bytes.is_some_and(|end| end > length)
                {
                    anyhow::bail!("inbox backlog cursor is outside its source file")
                }
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.mailbox(session_id))?;
            }
            BacklogPhase::Complete => {
                remove_if_exists(&self.backlog(session_id))?;
                remove_if_exists(&self.processing(session_id))?;
                remove_if_exists(&self.processing_tmp(session_id))?;
                remove_if_exists(&state_path)?;
            }
        }
        Ok(())
    }

    fn read_backlog_state_unlocked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<BacklogState>> {
        match read_bounded_text(&self.backlog_state(session_id), MAX_BACKLOG_STATE_BYTES) {
            Ok(contents) => Ok(Some(serde_json::from_str(&contents)?)),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn ensure_backlog_ready_unlocked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<BacklogState>> {
        self.recover_backlog_state_unlocked(session_id)?;
        let state_path = self.backlog_state(session_id);
        if state_path.exists() {
            return Ok(Some(serde_json::from_str(&read_bounded_text(
                &state_path,
                MAX_BACKLOG_STATE_BYTES,
            )?)?));
        }
        if self.backlog(session_id).exists() {
            anyhow::bail!("inbox backlog exists without its cursor state")
        }

        let source = if self.processing(session_id).exists()
            && std::fs::metadata(self.processing(session_id))?.len() > 0
        {
            BacklogSource::Processing
        } else if self.mailbox(session_id).exists()
            && std::fs::metadata(self.mailbox(session_id))?.len() > 0
        {
            BacklogSource::Mailbox
        } else {
            return Ok(None);
        };

        let state = BacklogState {
            source,
            phase: BacklogPhase::Preparing,
            cursor_bytes: 0,
            active_end_bytes: None,
            active_attempts: 0,
        };
        self.write_backlog_state_unlocked(session_id, &state)?;
        // Recovery uses the same transition path if the process stops between
        // the durable preparation record and the constant-time rename.
        self.recover_backlog_state_unlocked(session_id)?;
        Ok(Some(serde_json::from_str(&read_bounded_text(
            &self.backlog_state(session_id),
            MAX_BACKLOG_STATE_BYTES,
        )?)?))
    }

    fn write_backlog_state_unlocked(
        &self,
        session_id: &str,
        state: &BacklogState,
    ) -> anyhow::Result<()> {
        let path = self.backlog_state(session_id);
        let tmp = self.backlog_state_tmp(session_id);
        let contents = serde_json::to_vec(state)?;
        if contents.len() > MAX_BACKLOG_STATE_BYTES {
            anyhow::bail!("inbox backlog state exceeds its bounded byte budget")
        }
        let mut file = File::create(&tmp)?;
        file.write_all(&contents)?;
        file.flush()?;
        file.sync_all()?;
        remove_if_exists(&path)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    fn pending_bytes_unlocked(&self, session_id: &str) -> anyhow::Result<u64> {
        let mailbox_bytes = file_len_or_zero(&self.mailbox(session_id))?;
        let state_path = self.backlog_state(session_id);
        let backlog_bytes = if state_path.exists() {
            let state: BacklogState =
                serde_json::from_str(&read_bounded_text(&state_path, MAX_BACKLOG_STATE_BYTES)?)?;
            if state.phase != BacklogPhase::Ready {
                anyhow::bail!("inbox backlog is not in a deliverable state")
            }
            file_len_or_zero(&self.backlog(session_id))?.saturating_sub(state.cursor_bytes)
        } else {
            file_len_or_zero(&self.processing(session_id))?
        };
        Ok(mailbox_bytes.saturating_add(backlog_bytes))
    }

    fn read_processing_unlocked(&self, session_id: &str) -> anyhow::Result<Vec<Envelope>> {
        let path = self.processing(session_id);
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        if content.len() > MAX_PROCESSING_BYTES {
            anyhow::bail!("processing batch exceeds its bounded byte budget")
        }
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| parse_envelope(session_id, line.trim()))
            .collect()
    }

    fn advance_backlog_cursor_unlocked(
        &self,
        session_id: &str,
        state: &mut BacklogState,
        end_bytes: u64,
    ) -> anyhow::Result<()> {
        self.write_processing_unlocked(session_id, &[])?;
        state.cursor_bytes = end_bytes;
        state.active_end_bytes = None;
        state.active_attempts = 0;
        if end_bytes >= std::fs::metadata(self.backlog(session_id))?.len() {
            state.phase = BacklogPhase::Complete;
        }
        self.write_backlog_state_unlocked(session_id, state)?;
        self.recover_backlog_state_unlocked(session_id)
    }
}

/// Explicit operator-only recovery facade. This is intentionally separate
/// from the model-facing `agent` tool, because quarantine advances a durable
/// inbox cursor and must require a reviewed row fingerprint.
#[derive(Debug, Clone)]
pub struct InboxRecovery {
    bus: InboxBus,
}

impl InboxRecovery {
    /// Open recovery operations against Haven's configured inbox directory.
    pub fn default_root() -> Self {
        Self {
            bus: InboxBus::default_root(),
        }
    }

    /// Return metadata for the next oversized pending row, if one blocks the
    /// recipient's current backlog cursor.
    pub fn inspect_next_oversized_row(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<OversizedInboxRow>> {
        self.bus.inspect_next_oversized_row(session_id)
    }

    /// Preserve the exact reviewed row in quarantine and move the durable
    /// backlog cursor past it.
    pub fn quarantine_next_oversized_row(
        &self,
        session_id: &str,
        expected_offset_bytes: u64,
        expected_sha256: &str,
    ) -> anyhow::Result<QuarantinedInboxRow> {
        self.bus
            .quarantine_next_oversized_row(session_id, expected_offset_bytes, expected_sha256)
    }
}

fn inspect_oversized_row(
    path: &Path,
    offset_bytes: u64,
) -> anyhow::Result<Option<OversizedInboxRow>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if offset_bytes >= file.metadata()?.len() {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset_bytes))?;
    let mut hasher = Sha256::new();
    let mut row_bytes = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let remaining = MAX_QUARANTINED_ROW_BYTES
            .saturating_add(1)
            .saturating_sub(row_bytes);
        if remaining == 0 {
            anyhow::bail!(
                "the blocking inbox row exceeds the {MAX_QUARANTINED_ROW_BYTES}-byte quarantine row limit; back up and reset the inbox to recover it"
            )
        }
        let read_limit = buffer.len().min(remaining as usize);
        let count = file.read(&mut buffer[..read_limit])?;
        if count == 0 {
            break;
        }
        let newline = buffer[..count].iter().position(|byte| *byte == b'\n');
        let through_newline = newline.map_or(count, |index| index + 1);
        hasher.update(&buffer[..through_newline]);
        row_bytes = row_bytes.saturating_add(through_newline as u64);
        if row_bytes > MAX_QUARANTINED_ROW_BYTES {
            anyhow::bail!(
                "the blocking inbox row exceeds the {MAX_QUARANTINED_ROW_BYTES}-byte quarantine row limit; back up and reset the inbox to recover it"
            )
        }
        if newline.is_some() {
            break;
        }
    }
    if row_bytes == 0 || row_bytes <= MAX_ENVELOPE_LINE_BYTES as u64 {
        return Ok(None);
    }
    Ok(Some(OversizedInboxRow {
        offset_bytes,
        row_bytes,
        sha256: format!("{:x}", hasher.finalize()),
    }))
}

fn copy_row_bounded(
    input: &mut impl Read,
    output: &mut impl Write,
    row_bytes: u64,
    maximum_bytes: u64,
) -> anyhow::Result<String> {
    if row_bytes > maximum_bytes {
        anyhow::bail!("inbox quarantine row exceeds its bounded copy limit")
    }
    let mut hasher = Sha256::new();
    let mut remaining = row_bytes;
    let mut buffer = [0u8; 8192];
    while remaining > 0 {
        let read_limit = buffer.len().min(remaining as usize);
        let count = input.read(&mut buffer[..read_limit])?;
        if count == 0 {
            anyhow::bail!("inbox source row ended before its inspected byte length")
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn verify_file_digest(
    path: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
) -> anyhow::Result<()> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() != expected_bytes || expected_bytes > MAX_QUARANTINED_ROW_BYTES {
        anyhow::bail!("existing inbox quarantine evidence has an invalid byte length")
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256 {
        anyhow::bail!("existing inbox quarantine evidence failed its SHA-256 check")
    }
    Ok(())
}

fn quarantine_bytes_in_root(root: &Path) -> anyhow::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if quarantine_file_parts(&name).is_some() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

fn validate_sha256(value: &str) -> anyhow::Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        anyhow::bail!("sha256 must be 64 lowercase hexadecimal characters")
    }
    Ok(())
}

fn quarantine_file_parts(file_name: &str) -> Option<(String, u64, String, bool)> {
    let (session_id, suffix) = file_name.split_once(".jsonl.quarantine.")?;
    validate_session_id(session_id).ok()?;
    let (suffix, is_tmp) = suffix
        .strip_suffix(".tmp")
        .map_or((suffix, false), |suffix| (suffix, true));
    let suffix = suffix.strip_suffix(".row")?;
    let (offset, sha256) = suffix.split_once('.')?;
    if offset.is_empty() || !offset.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let offset = offset.parse().ok()?;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some((session_id.to_string(), offset, sha256.to_string(), is_tmp))
}

fn parse_deleted_sessions(contents: &str) -> anyhow::Result<HashMap<String, DeletedSessionEntry>> {
    let deleted: HashMap<String, DeletedSessionEntry> = serde_json::from_str(contents)
        .map_err(|error| anyhow::anyhow!("corrupt deleted_sessions.json: {error}"))?;
    for (session_id, entry) in &deleted {
        validate_session_id(session_id).map_err(|error| {
            anyhow::anyhow!("invalid session id in deleted_sessions.json: {error}")
        })?;
        if let Some(parent_session_id) = entry.parent_session_id.as_deref() {
            validate_session_id(parent_session_id).map_err(|error| {
                anyhow::anyhow!("invalid parent session ID in deleted_sessions.json: {error}")
            })?;
        }
    }
    Ok(deleted)
}

fn parse_deleted_sessions_file(
    path: &Path,
) -> anyhow::Result<HashMap<String, DeletedSessionEntry>> {
    let contents = read_bounded_text(path, MAX_DELETED_SESSIONS_BYTES)?;
    if contents.trim().is_empty() {
        return Ok(HashMap::new());
    }
    parse_deleted_sessions(&contents)
}

fn validate_deleted_sessions_size(byte_len: usize) -> anyhow::Result<()> {
    if byte_len > MAX_DELETED_SESSIONS_BYTES {
        anyhow::bail!(
            "deleted session lineage exceeds its bounded byte budget of {MAX_DELETED_SESSIONS_BYTES} bytes"
        )
    }
    Ok(())
}

fn is_transient_file_read_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() != std::io::ErrorKind::InvalidData)
}

fn session_id_from_inbox_file_name(file_name: &str) -> Option<String> {
    for suffix in [
        ".jsonl.backlog.state.json.tmp",
        ".jsonl.backlog.state.json",
        ".jsonl.backlog",
        ".jsonl.processing.tmp",
        ".jsonl.processing",
        ".archive.jsonl.tmp",
        ".archive.jsonl",
        ".jsonl.tmp",
        ".jsonl",
    ] {
        if let Some(session_id) = file_name.strip_suffix(suffix)
            && validate_session_id(session_id).is_ok()
        {
            return Some(session_id.to_string());
        }
    }
    quarantine_file_parts(file_name).map(|(session_id, _, _, _)| session_id)
}

fn append_history_source(
    session_id: &str,
    content: &str,
    entries: &mut Vec<Envelope>,
    positions: &mut HashMap<String, usize>,
) -> anyhow::Result<()> {
    for line in content.lines() {
        if line.len() > MAX_MESSAGE_ENVELOPE_BYTES {
            anyhow::bail!(
                "messaging JSONL row exceeds the bounded read limit of {MAX_MESSAGE_ENVELOPE_BYTES} bytes; the source row is preserved"
            )
        }
        let Ok(envelope) = parse_envelope(session_id, line.trim()) else {
            continue;
        };
        if let Some(index) = positions.get(&envelope.message_id).copied() {
            entries[index] = envelope;
        } else {
            positions.insert(envelope.message_id.clone(), entries.len());
            entries.push(envelope);
        }
    }
    Ok(())
}

fn find_in_jsonl_bounded(
    session_id: &str,
    path: &Path,
    start_bytes: u64,
    max_bytes: u64,
    message_id: &str,
) -> anyhow::Result<Option<Envelope>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let length = file.metadata()?.len();
    if start_bytes >= length {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(start_bytes))?;
    let mut reader = BufReader::new(file);
    let mut scanned = 0u64;
    loop {
        let Some(line) = read_bounded_line(&mut reader)? else {
            break;
        };
        let line_bytes = line.len() as u64;
        if scanned > 0 && scanned.saturating_add(line_bytes) > max_bytes {
            break;
        }
        scanned = scanned.saturating_add(line_bytes);
        if let Ok(line) = std::str::from_utf8(&line)
            && let Some(envelope) = parse_envelope(session_id, line.trim())
                .ok()
                .filter(|envelope| envelope.message_id == message_id)
        {
            return Ok(Some(envelope));
        }
        if scanned >= max_bytes || reader.stream_position()? >= length {
            break;
        }
    }
    Ok(None)
}

fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_bounded_text(path: &Path, max_bytes: usize) -> anyhow::Result<String> {
    let reader = File::open(path)?.take(max_bytes as u64 + 1);
    let mut contents = String::new();
    let mut reader = reader;
    reader.read_to_string(&mut contents)?;
    if contents.len() > max_bytes {
        anyhow::bail!("persistent inbox state exceeds its bounded byte budget")
    }
    Ok(contents)
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn file_len_or_zero(path: &Path) -> anyhow::Result<u64> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

fn select_active_envelopes(
    envelopes: &[Envelope],
    archived_ids: &HashSet<String>,
) -> Vec<Envelope> {
    let mut seen = HashSet::new();
    envelopes
        .iter()
        .filter(|envelope| {
            !is_expired(envelope)
                && seen.insert(envelope.message_id.clone())
                && (envelope.delivery_attempt > 0 || !archived_ids.contains(&envelope.message_id))
        })
        .cloned()
        .map(|mut envelope| {
            envelope.delivery_attempt = envelope.delivery_attempt.saturating_add(1);
            envelope
        })
        .collect()
}

fn select_at_attempts(envelopes: &[Envelope], attempts: u32) -> Vec<Envelope> {
    let mut seen = HashSet::new();
    envelopes
        .iter()
        .filter(|envelope| !is_expired(envelope) && seen.insert(envelope.message_id.clone()))
        .cloned()
        .map(|mut envelope| {
            envelope.delivery_attempt = envelope.delivery_attempt.saturating_add(attempts);
            envelope
        })
        .collect()
}

fn inferred_attempts(source: &[Envelope], processing: &[Envelope]) -> u32 {
    let source_attempts: HashMap<&str, u32> = source
        .iter()
        .map(|envelope| (envelope.message_id.as_str(), envelope.delivery_attempt))
        .collect();
    processing
        .iter()
        .filter_map(|envelope| {
            source_attempts
                .get(envelope.message_id.as_str())
                .map(|source_attempt| envelope.delivery_attempt.saturating_sub(*source_attempt))
        })
        .max()
        .unwrap_or_default()
}

fn read_bounded_line(reader: &mut impl BufRead) -> anyhow::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > MAX_ENVELOPE_LINE_BYTES {
            anyhow::bail!(
                "messaging JSONL row exceeds the bounded read limit of {MAX_ENVELOPE_LINE_BYTES} bytes; the source row is preserved; inspect it with `haven-inbox-recovery inspect <session_id>`"
            )
        }
        let has_newline = available[take - 1] == b'\n';
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if has_newline {
            return Ok(Some(line));
        }
    }
}

fn read_backlog_batch(
    session_id: &str,
    path: &Path,
    start_bytes: u64,
) -> anyhow::Result<Option<(u64, Vec<Envelope>)>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("inbox backlog source is missing")
        }
        Err(error) => return Err(error.into()),
    };
    let file_len = file.metadata()?.len();
    if start_bytes > file_len {
        anyhow::bail!("inbox backlog cursor is outside its source file")
    }
    file.seek(SeekFrom::Start(start_bytes))?;
    let mut reader = BufReader::new(file);
    let mut rows = 0usize;
    let mut bytes = 0u64;
    let mut envelopes = Vec::new();
    loop {
        if rows >= MAX_CLAIM_BATCH_LINES {
            break;
        }
        let line_start = reader.stream_position()?;
        let Some(line) = read_bounded_line(&mut reader)? else {
            break;
        };
        let line_len = line.len() as u64;
        if rows > 0 && bytes.saturating_add(line_len) > MAX_CLAIM_BATCH_BYTES {
            reader.seek(SeekFrom::Start(line_start))?;
            break;
        }
        rows += 1;
        bytes = bytes.saturating_add(line_len);
        let line = std::str::from_utf8(&line)?;
        match parse_envelope(session_id, line.trim()) {
            Ok(envelope) => envelopes.push(envelope),
            Err(error) => tracing::warn!(
                session_id,
                line_bytes = line.len(),
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "inbox: skipping invalid message envelope"
            ),
        }
        if reader.stream_position()? >= file_len {
            break;
        }
    }
    if rows == 0 {
        return Ok(None);
    }
    Ok(Some((reader.stream_position()?, envelopes)))
}

fn read_backlog_range(
    session_id: &str,
    path: &Path,
    start_bytes: u64,
    end_bytes: u64,
) -> anyhow::Result<Vec<Envelope>> {
    if end_bytes < start_bytes || end_bytes.saturating_sub(start_bytes) > MAX_CLAIM_BATCH_BYTES {
        anyhow::bail!("inbox active batch range is invalid or exceeds its byte budget")
    }
    let mut file = File::open(path)?;
    if end_bytes > file.metadata()?.len() {
        anyhow::bail!("inbox active batch range is outside its source file")
    }
    file.seek(SeekFrom::Start(start_bytes))?;
    let mut reader = BufReader::new(file);
    let mut envelopes = Vec::new();
    while reader.stream_position()? < end_bytes {
        let Some(line) = read_bounded_line(&mut reader)? else {
            anyhow::bail!("inbox active batch ended before its durable cursor")
        };
        if reader.stream_position()? > end_bytes {
            anyhow::bail!("inbox active batch cursor does not end on a row boundary")
        }
        let line = std::str::from_utf8(&line)?;
        if let Ok(envelope) = parse_envelope(session_id, line.trim()) {
            envelopes.push(envelope);
        }
    }
    Ok(envelopes)
}

fn parse_envelope(session_id: &str, line: &str) -> anyhow::Result<Envelope> {
    if line.len() > MAX_MESSAGE_ENVELOPE_BYTES {
        anyhow::bail!(
            "messaging JSONL row exceeds the bounded read limit of {MAX_MESSAGE_ENVELOPE_BYTES} bytes; the source row is preserved"
        )
    }
    let envelope: Envelope = serde_json::from_str(line)
        .map_err(|error| anyhow::anyhow!("corrupt message envelope: {error}"))?;
    validate_message_envelope(session_id, &envelope)?;
    Ok(envelope)
}

fn now_rfc3339() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn is_online(last_seen: &str, now: DateTime<Local>) -> bool {
    DateTime::parse_from_rfc3339(last_seen)
        .map(|ts| {
            let ts = ts.with_timezone(&now.timezone());
            now.signed_duration_since(ts)
                <= chrono::Duration::from_std(OFFLINE_AFTER).unwrap_or_default()
        })
        .unwrap_or(false)
}

/// Whether `env` is a substantive reply to the given request id from the
/// expected peer (excludes auto receipts, which also set `in_reply_to_message_id`).
fn is_reply_to_request(
    env: &Envelope,
    in_reply_to_message_id: &str,
    expected_from_session_id: &str,
) -> bool {
    env.in_reply_to_message_id.as_deref() == Some(in_reply_to_message_id)
        && env.from_session_id == expected_from_session_id
        && matches!(env.r#type, MessageType::Reply | MessageType::Message)
}

/// Expired when `expires_at` is set, parses, and is in the past. Unparseable
/// or absent `expires_at` is treated as not expired.
/// Read at most the last `max_bytes` of a file as UTF-8. Missing files are
/// treated as empty because an archive/mailbox is created lazily; other I/O
/// failures propagate so permission/disk errors cannot silently disable reply
/// resolution or deduplication.
fn read_tail(path: &Path, max_bytes: u64) -> anyhow::Result<String> {
    read_tail_from(path, 0, max_bytes)
}

fn read_tail_from(path: &Path, lower_bound: u64, max_bytes: u64) -> anyhow::Result<String> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.into()),
    };
    let len = f.metadata()?.len();
    if lower_bound >= len {
        return Ok(String::new());
    }
    let start = lower_bound.max(len.saturating_sub(max_bytes));
    f.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    let mut s = String::from_utf8_lossy(&bytes).into_owned();
    // The seek can land in the middle of a JSONL record (and, for UTF-8,
    // the middle of a code point). Discard that partial first record so every
    // caller sees only complete lines from the bounded tail.
    if start > lower_bound {
        if let Some(newline) = s.find('\n') {
            s.drain(..=newline);
        } else {
            anyhow::bail!(
                "messaging JSONL row exceeds the bounded read window; the source row is preserved"
            )
        }
    }
    Ok(s)
}

fn last_valid_line(session_id: &str, path: &Path) -> anyhow::Result<Option<Envelope>> {
    last_valid_line_from(session_id, path, 0)
}

fn last_valid_line_from(
    session_id: &str,
    path: &Path,
    lower_bound: u64,
) -> anyhow::Result<Option<Envelope>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let length = file.metadata()?.len();
    if lower_bound >= length {
        return Ok(None);
    }
    let max_line_bytes = MAX_ENVELOPE_LINE_BYTES as u64;
    let start = if length - lower_bound > max_line_bytes {
        length - max_line_bytes
    } else {
        lower_bound
    };
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::with_capacity((length - start) as usize);
    file.read_to_end(&mut tail)?;
    if start > lower_bound {
        let Some(newline) = tail.iter().position(|byte| *byte == b'\n') else {
            anyhow::bail!(
                "messaging JSONL row exceeds the bounded read limit of {MAX_ENVELOPE_LINE_BYTES} bytes; the source row is preserved"
            )
        };
        tail.drain(..=newline);
    }
    if tail.last() == Some(&b'\n') {
        tail.pop();
    }
    let line_start = tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    let line = &tail[line_start..];
    if line.len() > MAX_MESSAGE_ENVELOPE_BYTES {
        anyhow::bail!(
            "messaging JSONL row exceeds the bounded read limit of {MAX_ENVELOPE_LINE_BYTES} bytes; the source row is preserved"
        )
    }
    if line.is_empty() {
        return Ok(None);
    }
    let line = std::str::from_utf8(line)?;
    Ok(parse_envelope(session_id, line.trim()).ok())
}

/// Cross-process file mutex held by the operating system. The `.lock` file is
/// persistent: removing it could let another process lock a different inode
/// while a holder still owns the original one. Dropping the handle releases
/// the OS lock after normal return, error, or process exit.
struct LockGuard {
    _file: File,
}

impl LockGuard {
    fn open_lock_file(root: &Path) -> anyhow::Result<(PathBuf, File)> {
        std::fs::create_dir_all(root)?;
        let path = root.join(".lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        Ok((path, file))
    }

    /// Try one OS lock acquisition without waiting. A live contender
    /// immediately returns `Ok(None)`.
    fn try_acquire(root: &Path) -> anyhow::Result<Option<Self>> {
        let (_, file) = Self::open_lock_file(root)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn acquire(root: &Path) -> anyhow::Result<Self> {
        let (path, file) = Self::open_lock_file(root)?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        anyhow::bail!("inbox lock timed out: another agent holds {path:?}");
                    }
                    std::thread::sleep(LOCK_WAIT);
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{MAX_MESSAGE_PAYLOAD_BYTES, MAX_MESSAGE_TEXT_BYTES};
    use serde_json::json;

    fn test_bus() -> (tempfile::TempDir, InboxBus) {
        let dir = tempfile::tempdir().unwrap();
        let bus = InboxBus::new(dir.path());
        (dir, bus)
    }

    fn claim_and_ack(bus: &InboxBus, session_id: &str) -> Vec<Envelope> {
        let messages = bus.claim_and_archive(session_id).unwrap();
        let ids = messages
            .iter()
            .map(|message| message.message_id.clone())
            .collect::<Vec<_>>();
        bus.ack_claimed(session_id, &ids).unwrap();
        messages
    }

    fn drain_all(bus: &InboxBus, session_id: &str) -> Vec<Envelope> {
        let mut drained = Vec::new();
        for _ in 0..1000 {
            let batch = bus.claim_and_archive(session_id).unwrap();
            if batch.is_empty() {
                let pending_mailbox = file_len_or_zero(&bus.mailbox(session_id)).unwrap() > 0;
                let pending_backlog = bus.backlog_state(session_id).exists();
                if pending_mailbox || pending_backlog {
                    continue;
                }
                break;
            }
            let ids = batch
                .iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>();
            bus.ack_claimed(session_id, &ids).unwrap();
            drained.extend(batch);
        }
        drained
    }

    use std::sync::Arc;

    fn env_from(a: &str, to: &str, text: &str) -> Envelope {
        Envelope::new(a, to, text)
    }

    fn write_mailbox(bus: &InboxBus, session_id: &str, envs: &[Envelope]) {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(bus.mailbox(session_id))
            .unwrap();
        for e in envs {
            writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
        }
    }

    fn reg_entry(session_id: &str, last_seen: &str) -> AgentEntry {
        AgentEntry {
            session_id: session_id.into(),
            last_seen: last_seen.into(),
            started_at: "2026-01-01T00:00:00+08:00".into(),
            title: None,
            role: None,
            parent_session_id: None,
            capabilities: vec![],
        }
    }

    #[test]
    fn validate_session_id_accepts_canonical_ids() {
        for session_id in [
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-0123456789abcdef0123456789abcdef",
        ] {
            assert!(validate_session_id(session_id).is_ok(), "{session_id}");
        }
    }

    #[test]
    fn validate_session_id_rejects_noncanonical_ids() {
        for session_id in [
            "",
            "agent-alpha",
            "ses-abc123",
            "../evil",
            "a/b",
            "a\\b",
            "a.b",
            ".lock",
            "a b",
            "a b!",
            "x\ny",
            "a".repeat(65).as_str(),
            "中文",
        ] {
            assert!(validate_session_id(session_id).is_err(), "{session_id}");
        }
    }

    #[test]
    fn registry_rejects_noncanonical_persisted_session_ids() {
        let (_dir, bus) = test_bus();
        let registry = serde_json::json!({
            "ses-old-session_id": {
                "session_id": "ses-old-session_id",
                "last_seen": "2026-01-01T00:00:00Z",
                "started_at": "2026-01-01T00:00:00Z",
                "capabilities": []
            }
        });
        std::fs::write(
            bus.root().join("agents.json"),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();

        let error = bus.read_registry_unlocked().unwrap_err();

        assert!(error.to_string().contains("canonical session id"));
    }

    #[test]
    fn register_creates_mailbox_and_registry_entry() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &["x".into()])
            .unwrap();
        bus.register_with_profile(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &[],
            None,
            None,
            Some("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        assert!(bus.mailbox("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").exists());
        let reg: HashMap<String, AgentEntry> =
            serde_json::from_str(&std::fs::read_to_string(bus.root().join("agents.json")).unwrap())
                .unwrap();
        assert_eq!(
            reg["ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"].capabilities,
            vec!["x"]
        );
        assert_eq!(reg.len(), 1);
        let registry_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(bus.root().join("agents.json")).unwrap())
                .unwrap();
        let entry = &registry_json["ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"];
        assert_eq!(entry["session_id"], "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(entry.get("name").is_none());
        assert_eq!(
            entry["parent_session_id"],
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[test]
    fn durable_messaging_records_use_current_explicit_shape() {
        let envelope = Envelope::new(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "hello",
        );
        let value = serde_json::to_value(&envelope).unwrap();
        for field in ["message_id", "from_session_id", "to_session_id"] {
            assert!(value.get(field).is_some(), "missing {field}");
        }
        for legacy_field in ["id", "from", "to", "reply_address", "in_reply_to"] {
            assert!(
                value.get(legacy_field).is_none(),
                "legacy {legacy_field} persisted"
            );
        }

        let legacy_envelope = serde_json::json!({
            "id": "msg-00000000000000000000000000000001",
            "from": "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "to": "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "text": "hello",
            "created_at": "2026-01-01T00:00:00Z"
        });
        assert!(serde_json::from_value::<Envelope>(legacy_envelope).is_err());

        let mut envelope_without_type = value.clone();
        envelope_without_type
            .as_object_mut()
            .unwrap()
            .remove("type");
        assert!(serde_json::from_value::<Envelope>(envelope_without_type).is_err());

        let legacy_registry_entry = serde_json::json!({
            "name": "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "last_seen": "2026-01-01T00:00:00Z",
            "started_at": "2026-01-01T00:00:00Z"
        });
        assert!(serde_json::from_value::<AgentEntry>(legacy_registry_entry).is_err());

        let versioned_backlog = serde_json::json!({
            "version": 1,
            "source": "mailbox",
            "phase": "preparing",
            "cursor_bytes": 0,
            "active_end_bytes": null,
            "active_attempts": 0
        });
        assert!(serde_json::from_value::<BacklogState>(versioned_backlog).is_err());
    }

    #[test]
    fn register_second_call_acts_as_heartbeat_without_duplicate() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        let first_seen = {
            let reg = serde_json::from_str::<HashMap<String, AgentEntry>>(
                &std::fs::read_to_string(bus.root().join("agents.json")).unwrap(),
            )
            .unwrap();
            reg["ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
                .last_seen
                .clone()
        };
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        let reg = serde_json::from_str::<HashMap<String, AgentEntry>>(
            &std::fs::read_to_string(bus.root().join("agents.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(reg.len(), 1, "re-register must not duplicate the entry");
        assert!(reg["ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"].last_seen >= first_seen);
    }

    #[test]
    fn register_rejects_invalid_session_id_without_creating_files() {
        let (_dir, bus) = test_bus();
        assert!(bus.register("../evil", &[]).is_err());
        assert!(!bus.mailbox("../evil").exists());
    }

    #[test]
    fn send_and_read_roundtrip() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let mut env = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "schema 定了吗？",
        );
        env.subject = Some("需要你确认".into());
        env.payload = Some(json!({"file": "/path/x.rs"}));
        let outcome = bus
            .deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &env)
            .unwrap();
        assert!(outcome.delivered);
        assert_eq!(outcome.status, AgentStatus::Online);

        let msgs = drain_all(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(msgs.len(), 1);
        let got = &msgs[0];
        assert_eq!(got.message_id, env.message_id);
        assert_eq!(got.from_session_id, "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(got.to_session_id, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(got.text, "schema 定了吗？");
        assert_eq!(got.subject.as_deref(), Some("需要你确认"));
        assert_eq!(got.payload, Some(json!({"file": "/path/x.rs"})));
        assert!(
            DateTime::parse_from_rfc3339(&got.created_at).is_ok(),
            "created_at must be RFC3339, got {}",
            got.created_at
        );

        // Read-then-move: a second read yields nothing.
        assert!(claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").is_empty());
        // The archive keeps the full history for audit.
        let archive =
            std::fs::read_to_string(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")).unwrap();
        assert_eq!(archive.lines().count(), 1);
        // The drained mailbox is recreated: the agent can still receive.
        let outcome = bus
            .deliver(
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                &env_from(
                    "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "第二封",
                ),
            )
            .unwrap();
        assert!(outcome.delivered);
        assert_eq!(
            claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").len(),
            1
        );
    }

    #[test]
    fn archive_keeps_only_a_bounded_newest_tail() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let messages = (0..500)
            .map(|index| {
                env_from(
                    "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    &format!("{index}:{}", "x".repeat(10_000)),
                )
            })
            .collect::<Vec<_>>();
        write_mailbox(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &messages);

        let claimed = drain_all(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(claimed.len(), messages.len());
        assert!(
            std::fs::metadata(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
                .unwrap()
                .len()
                <= MAX_ARCHIVE_BYTES,
            "archive must stay within its byte budget"
        );
        let history = bus
            .history("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 1)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].message_id, messages.last().unwrap().message_id);
    }

    #[test]
    fn archive_tmp_is_recovered_after_interrupted_windows_replacement() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let archived = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "recovered archive",
        );
        std::fs::write(
            bus.archive_tmp("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            format!("{}\n", serde_json::to_string(&archived).unwrap()),
        )
        .unwrap();

        let history = bus
            .history("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 1)
            .unwrap();

        assert_eq!(history[0].message_id, archived.message_id);
        assert!(
            !bus.archive_tmp("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .exists()
        );
        assert!(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").exists());
    }

    #[test]
    fn archive_tmp_is_discarded_when_the_old_archive_survived() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let old = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "old archive",
        );
        let abandoned = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "abandoned replacement",
        );
        std::fs::write(
            bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            format!("{}\n", serde_json::to_string(&old).unwrap()),
        )
        .unwrap();
        std::fs::write(
            bus.archive_tmp("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            format!("{}\n", serde_json::to_string(&abandoned).unwrap()),
        )
        .unwrap();

        let history = bus
            .history("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 10)
            .unwrap();

        assert_eq!(
            history
                .iter()
                .map(|env| &env.message_id)
                .collect::<Vec<_>>(),
            vec![&old.message_id]
        );
        assert!(
            !bus.archive_tmp("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .exists()
        );
        assert_eq!(
            std::fs::read_to_string(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")).unwrap(),
            format!("{}\n", serde_json::to_string(&old).unwrap())
        );
    }

    #[test]
    fn read_paths_do_not_rewrite_a_bounded_archive() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let archived = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "stable archive",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&archived),
        );
        std::fs::rename(
            bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            bus.processing("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let before = std::fs::metadata(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        assert_eq!(
            bus.history("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 1)
                .unwrap()[0]
                .message_id,
            archived.message_id
        );
        assert_eq!(
            bus.find_message("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &archived.message_id)
                .unwrap()
                .unwrap()
                .message_id,
            archived.message_id
        );

        let after = std::fs::metadata(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            after, before,
            "bounded read paths must not rewrite the archive"
        );
    }

    #[test]
    fn reading_oversized_archive_does_not_rewrite_it() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let messages = (0..500)
            .map(|index| {
                env_from(
                    "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    &format!("{index}:{}", "x".repeat(10_000)),
                )
            })
            .collect::<Vec<_>>();
        let mut archive = OpenOptions::new()
            .create(true)
            .append(true)
            .open(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
            .unwrap();
        for message in &messages {
            writeln!(archive, "{}", serde_json::to_string(message).unwrap()).unwrap();
        }
        let archive_bytes_before =
            std::fs::metadata(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
                .unwrap()
                .len();
        assert!(archive_bytes_before > MAX_ARCHIVE_BYTES);

        let history = bus
            .history("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 1)
            .unwrap();

        assert_eq!(history[0].message_id, messages.last().unwrap().message_id);
        assert_eq!(
            std::fs::metadata(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
                .unwrap()
                .len(),
            archive_bytes_before
        );
    }

    #[test]
    fn claim_redelivers_until_ack_and_archives_once() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let env = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "durable",
        );
        bus.deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &env)
            .unwrap();

        let first = bus
            .claim_and_archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].message_id, env.message_id);

        // Simulate a process crash after claim/archive but before transcript
        // projection and acknowledgement.
        let redelivered = bus
            .claim_and_archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert_eq!(redelivered.len(), 1);
        assert_eq!(redelivered[0].message_id, env.message_id);

        bus.ack_claimed(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&env.message_id),
        )
        .unwrap();
        assert!(
            bus.claim_and_archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap()
                .is_empty()
        );
        let archive =
            std::fs::read_to_string(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")).unwrap();
        assert_eq!(archive.lines().count(), 1);
    }

    #[test]
    fn archive_failure_retries_the_same_active_backlog_range() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let envelope = env_from("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "retry");
        bus.deliver(recipient, &envelope).unwrap();
        bus.fail_archive_append
            .store(true, std::sync::atomic::Ordering::SeqCst);

        assert!(bus.claim_and_archive(recipient).is_err());
        let state_after_failure: BacklogState =
            serde_json::from_str(&std::fs::read_to_string(bus.backlog_state(recipient)).unwrap())
                .unwrap();
        assert!(state_after_failure.active_end_bytes.is_some());
        assert!(bus.backlog(recipient).exists());

        let retried = bus.claim_and_archive(recipient).unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].message_id, envelope.message_id);
        assert_eq!(retried[0].delivery_attempt, 2);
        let archive = std::fs::read_to_string(bus.archive(recipient)).unwrap();
        assert_eq!(archive.lines().count(), 1);
        bus.ack_claimed(recipient, &[envelope.message_id]).unwrap();
        assert!(bus.claim_and_archive(recipient).unwrap().is_empty());
    }

    #[test]
    fn missing_processing_copy_rebuilds_from_cursor_with_stable_message_id() {
        let (dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let envelope = env_from("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "recover");
        bus.deliver(recipient, &envelope).unwrap();
        let first = bus.claim_and_archive(recipient).unwrap();
        assert_eq!(first[0].delivery_attempt, 1);
        remove_if_exists(&bus.processing(recipient)).unwrap();

        let restarted_bus = InboxBus::new(dir.path());
        let recovered = restarted_bus.claim_and_archive(recipient).unwrap();

        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].message_id, envelope.message_id);
        assert_eq!(recovered[0].delivery_attempt, 1);
        restarted_bus
            .ack_claimed(recipient, std::slice::from_ref(&envelope.message_id))
            .unwrap();
        assert!(
            restarted_bus
                .claim_and_archive(recipient)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn preparing_cursor_recovers_the_source_rename_after_restart() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let envelope = env_from("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "prepare");
        bus.deliver(recipient, &envelope).unwrap();
        let state = BacklogState {
            source: BacklogSource::Mailbox,
            phase: BacklogPhase::Preparing,
            cursor_bytes: 0,
            active_end_bytes: None,
            active_attempts: 0,
        };
        std::fs::write(
            bus.backlog_state(recipient),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();

        let recovered = bus.claim_and_archive(recipient).unwrap();

        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].message_id, envelope.message_id);
        assert!(bus.backlog(recipient).exists());
        assert_eq!(file_len_or_zero(&bus.mailbox(recipient)).unwrap(), 0);
    }

    #[test]
    fn direct_delivery_rejects_noncanonical_envelope_before_write() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let mut envelope = env_from("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "hello");
        envelope.from_session_id = "ses-old-agent-session_id".into();

        let error = bus.deliver(recipient, &envelope).unwrap_err();

        assert!(error.to_string().contains("canonical session id"));
        assert!(
            std::fs::read_to_string(bus.mailbox(recipient))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn direct_delivery_rejects_oversized_text_before_write() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let mut envelope = env_from("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "hello");
        envelope.text = "x".repeat(MAX_MESSAGE_TEXT_BYTES + 1);

        let error = bus.deliver(recipient, &envelope).unwrap_err();

        assert!(error.to_string().contains("message text"));
        assert!(
            std::fs::read_to_string(bus.mailbox(recipient))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn delivery_rejects_when_cumulative_pending_bytes_exceed_session_budget() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let mut existing = vec![b'x'; MAX_PENDING_BYTES as usize - 1];
        *existing.last_mut().unwrap() = b'\n';
        std::fs::write(bus.mailbox(recipient), &existing).unwrap();
        let incoming = env_from("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "new");

        let error = bus.deliver(recipient, &incoming).unwrap_err();

        assert!(error.to_string().contains("inbox capacity exceeded"));
        assert_eq!(
            std::fs::metadata(bus.mailbox(recipient)).unwrap().len(),
            existing.len() as u64
        );
    }

    #[test]
    fn claims_are_bounded_and_cursor_advances_only_after_full_batch_ack() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let messages = (0..250)
            .map(|index| {
                env_from(
                    "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    recipient,
                    &format!("m{index}"),
                )
            })
            .collect::<Vec<_>>();
        write_mailbox(&bus, recipient, &messages);

        let first = bus.claim_and_archive(recipient).unwrap();
        assert_eq!(first.len(), MAX_CLAIM_BATCH_LINES);
        let state: BacklogState =
            serde_json::from_str(&std::fs::read_to_string(bus.backlog_state(recipient)).unwrap())
                .unwrap();
        assert_eq!(
            state.cursor_bytes, 0,
            "claim alone must not advance the cursor"
        );
        assert!(state.active_end_bytes.is_some());

        let first_half = first[..50]
            .iter()
            .map(|envelope| envelope.message_id.clone())
            .collect::<Vec<_>>();
        bus.ack_claimed(recipient, &first_half).unwrap();
        let retry = bus.claim_and_archive(recipient).unwrap();
        assert_eq!(
            retry.len(),
            50,
            "only unacked IDs from the active batch return"
        );
        assert!(retry.iter().all(|envelope| envelope.delivery_attempt == 2));
        let retry_ids = retry
            .iter()
            .map(|env| env.message_id.clone())
            .collect::<HashSet<_>>();
        let expected_ids = first[50..]
            .iter()
            .map(|env| env.message_id.clone())
            .collect::<HashSet<_>>();
        assert_eq!(retry_ids, expected_ids);
        bus.ack_claimed(
            recipient,
            &retry
                .iter()
                .map(|env| env.message_id.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();

        let second = bus.claim_and_archive(recipient).unwrap();
        assert_eq!(second.len(), MAX_CLAIM_BATCH_LINES);
        assert!(second.iter().all(|env| env.delivery_attempt == 1));
        bus.ack_claimed(
            recipient,
            &second
                .iter()
                .map(|env| env.message_id.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let third = bus.claim_and_archive(recipient).unwrap();
        assert_eq!(third.len(), 50);
        bus.ack_claimed(
            recipient,
            &third
                .iter()
                .map(|env| env.message_id.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(bus.claim_and_archive(recipient).unwrap().is_empty());
        assert!(!bus.backlog_state(recipient).exists());
    }

    #[test]
    fn oversized_jsonl_row_fails_closed_without_advancing_or_deleting_source() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let mut envelope = env_from("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", recipient, "legacy");
        envelope.text = "x".repeat(MAX_MESSAGE_ENVELOPE_BYTES + 1);
        write_mailbox(&bus, recipient, &[envelope]);
        let original_len = std::fs::metadata(bus.mailbox(recipient)).unwrap().len();

        let error = bus.claim_and_archive(recipient).unwrap_err();

        assert!(error.to_string().contains("bounded read limit"));
        assert_eq!(
            std::fs::metadata(bus.backlog(recipient)).unwrap().len(),
            original_len
        );
        let state: BacklogState =
            serde_json::from_str(&std::fs::read_to_string(bus.backlog_state(recipient)).unwrap())
                .unwrap();
        assert_eq!(state.cursor_bytes, 0);
        assert_eq!(state.active_end_bytes, None);
    }

    #[test]
    fn oversized_jsonl_row_can_be_quarantined_by_reviewed_offset_and_digest() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let mut oversized = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            recipient,
            "oversized",
        );
        oversized.text = "x".repeat(MAX_MESSAGE_ENVELOPE_BYTES + 1);
        let following = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            recipient,
            "deliver after quarantine",
        );
        write_mailbox(&bus, recipient, &[oversized, following.clone()]);
        let original = std::fs::read(bus.mailbox(recipient)).unwrap();

        let inspected = bus.inspect_next_oversized_row(recipient).unwrap().unwrap();
        assert_eq!(inspected.offset_bytes, 0);
        assert!(inspected.row_bytes > MAX_ENVELOPE_LINE_BYTES as u64);

        let stale = bus
            .quarantine_next_oversized_row(recipient, inspected.offset_bytes, &"0".repeat(64))
            .unwrap_err();
        assert!(stale.to_string().contains("row changed since inspection"));
        let state: BacklogState =
            serde_json::from_str(&std::fs::read_to_string(bus.backlog_state(recipient)).unwrap())
                .unwrap();
        assert_eq!(state.cursor_bytes, 0);

        let quarantined = bus
            .quarantine_next_oversized_row(recipient, inspected.offset_bytes, &inspected.sha256)
            .unwrap();
        assert_eq!(quarantined.row, inspected);
        assert_eq!(
            bus.quarantine_next_oversized_row(recipient, inspected.offset_bytes, &inspected.sha256)
                .unwrap(),
            quarantined,
            "repeating a completed quarantine request must be a no-op success"
        );
        assert_eq!(
            std::fs::read(&quarantined.quarantine_file).unwrap(),
            original[..inspected.row_bytes as usize]
        );
        let state: BacklogState =
            serde_json::from_str(&std::fs::read_to_string(bus.backlog_state(recipient)).unwrap())
                .unwrap();
        assert_eq!(state.cursor_bytes, inspected.row_bytes);

        let claim = bus.claim_and_archive(recipient).unwrap();
        assert_eq!(claim.len(), 1);
        assert_eq!(claim[0].message_id, following.message_id);
        bus.ack_claimed(recipient, &[following.message_id]).unwrap();
        assert!(quarantined.quarantine_file.exists());

        bus.delete_sessions(&[recipient.to_string()]).unwrap();
        assert!(!quarantined.quarantine_file.exists());
    }

    #[test]
    fn quarantine_retry_finishes_when_evidence_was_committed_before_the_cursor() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let mut oversized = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            recipient,
            "oversized",
        );
        oversized.text = "x".repeat(MAX_MESSAGE_ENVELOPE_BYTES + 1);
        write_mailbox(&bus, recipient, &[oversized]);
        let inspected = bus.inspect_next_oversized_row(recipient).unwrap().unwrap();

        // Simulate a crash after the evidence rename but before the cursor
        // sidecar commit. The retry verifies the committed copy and advances.
        let evidence = bus.quarantine_row(recipient, &inspected);
        let source = std::fs::read(bus.backlog(recipient)).unwrap();
        std::fs::write(&evidence, &source[..inspected.row_bytes as usize]).unwrap();
        let result = bus
            .quarantine_next_oversized_row(recipient, inspected.offset_bytes, &inspected.sha256)
            .unwrap();

        assert_eq!(result.row, inspected);
        assert!(
            !bus.backlog_state(recipient).exists(),
            "a fully drained backlog should remove its completed cursor state"
        );
        assert_eq!(std::fs::read(evidence).unwrap(), source);
    }

    #[test]
    fn inbox_recovery_refuses_to_skip_a_valid_or_active_row() {
        let (_dir, bus) = test_bus();
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let valid = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            recipient,
            "valid row",
        );
        write_mailbox(&bus, recipient, &[valid]);
        assert!(bus.inspect_next_oversized_row(recipient).unwrap().is_none());

        bus.claim_and_archive(recipient).unwrap();
        let error = bus
            .quarantine_next_oversized_row(recipient, 0, &"0".repeat(64))
            .unwrap_err();
        assert!(error.to_string().contains("unacknowledged bounded batch"));
    }

    #[test]
    fn inbox_session_enumeration_recognizes_quarantine_evidence() {
        let session_id = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let name = format!(
            "{session_id}.jsonl.quarantine.12.{}.row.tmp",
            "a".repeat(64)
        );
        assert_eq!(
            session_id_from_inbox_file_name(&name).as_deref(),
            Some(session_id)
        );
        assert!(
            session_id_from_inbox_file_name(&format!(
                "{session_id}.jsonl.quarantine.12.{}.row",
                "A".repeat(64)
            ))
            .is_none()
        );
    }

    #[test]
    fn inbox_recovery_bounds_the_row_scan_before_copying() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversized.jsonl");
        let mut file = File::create(&path).unwrap();
        let chunk = [b'x'; 8192];
        let mut remaining = MAX_QUARANTINED_ROW_BYTES as usize + 1;
        while remaining > 0 {
            let count = remaining.min(chunk.len());
            file.write_all(&chunk[..count]).unwrap();
            remaining -= count;
        }
        file.write_all(b"\n").unwrap();
        file.flush().unwrap();
        drop(file);
        let original_len = std::fs::metadata(&path).unwrap().len();

        let error = inspect_oversized_row(&path, 0).unwrap_err();

        assert!(error.to_string().contains("quarantine row limit"));
        assert_eq!(std::fs::metadata(path).unwrap().len(), original_len);
    }

    #[test]
    fn try_claim_returns_busy_without_waiting_for_global_lock() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let _held = LockGuard::acquire(bus.root()).unwrap();

        let started = std::time::Instant::now();
        let result = bus
            .try_claim_and_archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();

        assert!(
            result.is_none(),
            "a live lock must defer the background poll"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "try_claim must not inherit the 15-second blocking lock timeout"
        );
    }

    #[test]
    fn claim_preserves_fifo_when_messages_arrive_during_active_batch() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let first = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "first",
        );
        let second = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "second",
        );
        bus.deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &first)
            .unwrap();
        assert_eq!(
            bus.claim_and_archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap()
                .len(),
            1
        );

        bus.deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &second)
            .unwrap();
        let still_active = bus
            .claim_and_archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert_eq!(
            still_active
                .iter()
                .map(|env| env.message_id.as_str())
                .collect::<Vec<_>>(),
            [first.message_id.as_str()],
            "a newer mailbox row must wait behind the unacked immutable batch"
        );

        bus.ack_claimed(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&first.message_id),
        )
        .unwrap();
        let remaining = bus
            .claim_and_archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].message_id, second.message_id);
        bus.ack_claimed(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&second.message_id),
        )
        .unwrap();
    }

    #[test]
    fn claim_recovers_interrupted_processing_replacement() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let env = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "recover",
        );
        std::fs::write(
            bus.processing_tmp("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            format!("{}\n", serde_json::to_string(&env).unwrap()),
        )
        .unwrap();

        let claimed = bus
            .claim_and_archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].message_id, env.message_id);
        assert!(
            !bus.processing_tmp("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .exists()
        );
        bus.ack_claimed(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&env.message_id),
        )
        .unwrap();
    }

    #[test]
    fn send_to_unregistered_agent_errors() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        let err = bus
            .deliver(
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                &env_from(
                    "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "hi",
                ),
            )
            .unwrap_err();
        assert!(err.to_string().contains("not found or offline"), "{err}");
    }

    #[test]
    fn send_to_stale_agent_is_lenient_but_reports_offline() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        // Rewrite the registry so session-a looks stale (heartbeat > 300s ago).
        let old = (Local::now()
            - chrono::Duration::from_std(OFFLINE_AFTER).unwrap()
            - chrono::Duration::seconds(10))
        .to_rfc3339_opts(SecondsFormat::Secs, false);
        let mut reg = HashMap::new();
        reg.insert(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            reg_entry("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &old),
        );
        bus.write_registry_unlocked(&reg).unwrap();

        let outcome = bus
            .deliver(
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                &env_from(
                    "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "hi",
                ),
            )
            .unwrap();
        assert!(
            outcome.delivered,
            "stale agents still receive (lenient delivery)"
        );
        assert_eq!(outcome.status, AgentStatus::Offline);
        // The message is there when session-b eventually polls.
        let msgs = claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn expired_messages_are_not_returned_but_still_archived() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let mut env = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "过期了",
        );
        env.expires_at = Some(
            (Local::now() - chrono::Duration::minutes(1))
                .to_rfc3339_opts(SecondsFormat::Secs, false),
        );
        bus.deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &env)
            .unwrap();
        let msgs = claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(msgs.is_empty(), "expired messages must be filtered");
        let archive =
            std::fs::read_to_string(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")).unwrap();
        assert_eq!(archive.lines().count(), 1, "but archived for audit");
    }

    #[test]
    fn future_expiry_is_returned() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let mut env = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "还有效",
        );
        env.expires_at = Some(
            (Local::now() + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, false),
        );
        bus.deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &env)
            .unwrap();
        assert_eq!(
            claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").len(),
            1
        );
    }

    #[test]
    fn corrupt_lines_are_skipped_without_losing_others() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let good = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ok",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&good),
        );
        let mut f = OpenOptions::new()
            .append(true)
            .open(bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"))
            .unwrap();
        writeln!(f, "{{not json").unwrap();
        let mut legacy_identity = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "legacy identity",
        );
        legacy_identity.from_session_id = "ses-old-agent-session_id".into();
        writeln!(f, "{}", serde_json::to_string(&legacy_identity).unwrap()).unwrap();
        writeln!(
            f,
            "{}",
            serde_json::to_string(&env_from(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "ok2"
            ))
            .unwrap()
        )
        .unwrap();
        drop(f);
        let msgs = claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].message_id, good.message_id);
    }

    #[test]
    fn crash_recovery_drains_leftover_processing() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let env = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "crash 前写的",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&env),
        );
        // Simulate a crash after the mailbox → .processing rename.
        std::fs::rename(
            bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            bus.processing("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        let msgs = claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(msgs.len(), 1, "leftover .processing must be drained");
        assert!(claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").is_empty());
        let archive =
            std::fs::read_to_string(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")).unwrap();
        assert_eq!(archive.lines().count(), 1);
    }

    #[test]
    fn crash_recovery_with_new_writes_after_crash() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let first = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "第一次",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&first),
        );
        std::fs::rename(
            bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            bus.processing("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        // New writes land in the fresh mailbox after the crash.
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &[env_from(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "第二次",
            )],
        );
        let msgs = drain_all(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(
            msgs.len(),
            2,
            "both the leftover and the new writes must be read"
        );
        assert_eq!(msgs[0].message_id, first.message_id);
    }

    #[test]
    fn archive_dedupes_after_rearchiving_same_processing() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let env = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "同一封",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&env),
        );
        std::fs::rename(
            bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            bus.processing("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        // Crash AFTER archiving but BEFORE deleting .processing: the message
        // is already in the archive. Re-reading must not duplicate it — in
        // the archive NOR in what is returned to the agent.
        let msgs = claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(msgs.len(), 1, "first drain returns the message");
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&env),
        );
        std::fs::rename(
            bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            bus.processing("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        let msgs = claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(
            msgs.is_empty(),
            "a message already archived must not be returned twice"
        );
        let archive =
            std::fs::read_to_string(bus.archive("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")).unwrap();
        assert_eq!(
            archive.lines().count(),
            1,
            "the same message id must not be archived twice"
        );
    }

    #[test]
    fn concurrent_delivers_produce_no_corrupt_lines() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let bus = Arc::new(bus);
        let mut handles = Vec::new();
        for t in 0..8 {
            let bus = bus.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..25 {
                    bus.deliver(
                        "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        &env_from(
                            &format!("ses-{t:032x}"),
                            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            &format!("m{t}-{i}"),
                        ),
                    )
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let msgs = drain_all(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(
            msgs.len(),
            8 * 25,
            "every concurrent write must survive intact"
        );
        let ids: HashSet<String> = msgs.iter().map(|e| e.message_id.clone()).collect();
        assert_eq!(ids.len(), msgs.len(), "ids must be unique");
    }

    #[test]
    fn list_agents_reports_online_and_offline() {
        let (_dir, bus) = test_bus();
        bus.register("ses-dddddddddddddddddddddddddddddddd", &[])
            .unwrap();
        bus.register("ses-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", &[])
            .unwrap();
        // Age out the second session's heartbeat in place (both entries kept).
        let old = (Local::now()
            - chrono::Duration::from_std(OFFLINE_AFTER).unwrap()
            - chrono::Duration::seconds(10))
        .to_rfc3339_opts(SecondsFormat::Secs, false);
        let mut reg = bus.read_registry_unlocked().unwrap();
        reg.get_mut("ses-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
            .unwrap()
            .last_seen = old;
        bus.write_registry_unlocked(&reg).unwrap();

        let agents = bus.list_agents().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(
            agents[0].session_id, "ses-dddddddddddddddddddddddddddddddd",
            "online agents come first"
        );
        assert_eq!(agents[0].status, AgentStatus::Online);
        assert_eq!(agents[1].status, AgentStatus::Offline);
    }

    #[test]
    fn last_received_prefers_mailbox_then_archive_and_honors_reply_session_id() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        // Archive history: an old message from session-a.
        let old = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "旧消息",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&old),
        );
        std::fs::rename(
            bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            bus.processing("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        // New pending message from session-c with a custom reply_to_session_id.
        let mut fresh = env_from(
            "ses-cccccccccccccccccccccccccccccccc",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "新消息",
        );
        fresh.reply_to_session_id = Some("ses-99999999999999999999999999999999".into());
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&fresh),
        );

        let last = bus
            .last_received("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap()
            .unwrap();
        assert_eq!(
            last.message_id, fresh.message_id,
            "mailbox entries are newer than archive entries"
        );
        assert_eq!(
            last.reply_target_session_id(),
            "ses-99999999999999999999999999999999",
            "reply_to_session_id overrides from"
        );

        // After reading the mailbox, the archive tail provides the last sender.
        claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let last = bus
            .last_received("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap()
            .unwrap();
        assert_eq!(last.message_id, fresh.message_id);
        assert_eq!(
            last.reply_target_session_id(),
            "ses-99999999999999999999999999999999"
        );
    }

    #[test]
    fn last_received_empty_when_no_history() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        assert!(
            bus.last_received("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn last_received_can_parse_a_maximum_sized_new_envelope() {
        let (_dir, bus) = test_bus();
        let sender = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(recipient, &[]).unwrap();
        let mut envelope = env_from(sender, recipient, &"\0".repeat(MAX_MESSAGE_TEXT_BYTES));
        envelope.payload = Some(json!("x".repeat(MAX_MESSAGE_PAYLOAD_BYTES - 2)));
        validate_message_envelope_for_delivery(recipient, &envelope).unwrap();
        assert!(serde_json::to_vec(&envelope).unwrap().len() > 64 * 1024);
        bus.deliver(recipient, &envelope).unwrap();

        let last = bus.last_received(recipient).unwrap().unwrap();

        assert_eq!(last.message_id, envelope.message_id);
    }

    #[test]
    fn find_message_searches_mailbox_and_archive() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let archived = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "已读",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&archived),
        );
        std::fs::rename(
            bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            bus.processing("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let fresh = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "未读",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&fresh),
        );

        let found = bus
            .find_message("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &fresh.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.message_id, fresh.message_id);
        let found = bus
            .find_message("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &archived.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.message_id, archived.message_id);
        assert!(
            bus.find_message("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "msg-nonexistent")
                .is_err()
        );
    }

    #[test]
    fn os_lock_is_exclusive_and_released_by_dropping_handle() {
        let (_dir, bus) = test_bus();
        let lock_path = bus.root().join(".lock");
        let guard = LockGuard::acquire(bus.root()).unwrap();
        assert!(lock_path.exists());
        assert!(LockGuard::try_acquire(bus.root()).unwrap().is_none());

        drop(guard);
        assert!(lock_path.exists(), "the lock file inode is persistent");

        let next_guard = LockGuard::try_acquire(bus.root())
            .unwrap()
            .expect("dropping the first handle must release the OS lock");
        drop(next_guard);
        assert!(lock_path.exists(), "release must not remove the lock file");
    }

    #[test]
    fn mailbox_paths_follow_the_spec_layout() {
        let (_dir, bus) = test_bus();
        assert_eq!(
            bus.mailbox("ses-ebf7891451d2d7f9e5c1bf89d169e552"),
            bus.root()
                .join("ses-ebf7891451d2d7f9e5c1bf89d169e552.jsonl")
        );
        assert_eq!(
            bus.archive("ses-ebf7891451d2d7f9e5c1bf89d169e552"),
            bus.root()
                .join("ses-ebf7891451d2d7f9e5c1bf89d169e552.archive.jsonl")
        );
        assert_eq!(
            bus.processing("ses-ebf7891451d2d7f9e5c1bf89d169e552"),
            bus.root()
                .join("ses-ebf7891451d2d7f9e5c1bf89d169e552.jsonl.processing")
        );
    }

    #[test]
    fn unregister_removes_entry_but_keeps_mailbox() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        bus.unregister("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        let names: Vec<String> = bus
            .list_agents()
            .unwrap()
            .into_iter()
            .map(|a| a.session_id)
            .collect();
        assert_eq!(
            names,
            vec!["ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()],
            "unregistered agent disappears"
        );
        // Mailbox survives: late messages are still deliverable (offline) and
        // the history is not lost.
        let outcome = bus
            .deliver(
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                &env_from(
                    "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "迟到的信",
                ),
            )
            .unwrap();
        assert!(outcome.delivered);
        assert_eq!(outcome.status, AgentStatus::Offline);
        assert_eq!(
            claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").len(),
            1
        );
        // Re-registering restores online status with history intact.
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        assert!(
            bus.list_agents()
                .unwrap()
                .iter()
                .any(|a| a.session_id == "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn unregister_unknown_session_is_noop() {
        let (_dir, bus) = test_bus();
        bus.unregister("ses-00000000000000000000000000000003")
            .unwrap();
    }

    #[test]
    fn mark_offline_keeps_parent_metadata() {
        let (_dir, bus) = test_bus();
        bus.register_with_profile(
            "ses-00000000000000000000000000000001",
            &["docs".into()],
            Some("worker"),
            Some("coder"),
            Some("ses-00000000000000000000000000000005"),
        )
        .unwrap();
        bus.mark_offline("ses-00000000000000000000000000000001")
            .unwrap();
        let child = bus
            .list_agents()
            .unwrap()
            .into_iter()
            .find(|agent| agent.session_id == "ses-00000000000000000000000000000001")
            .unwrap();
        assert_eq!(child.status, AgentStatus::Offline);
        assert_eq!(
            child.parent_session_id.as_deref(),
            Some("ses-00000000000000000000000000000005")
        );
        assert_eq!(child.capabilities, vec!["docs"]);
    }

    #[test]
    fn register_with_title_sets_and_preserves_title() {
        let (_dir, bus) = test_bus();
        bus.register_with_title(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &[],
            Some("修复登录 bug"),
        )
        .unwrap();
        let title = bus
            .list_agents()
            .unwrap()
            .into_iter()
            .find(|a| a.session_id == "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap()
            .title;
        assert_eq!(title.as_deref(), Some("修复登录 bug"));
        // A title-less heartbeat keeps the stored title…
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        let title = bus.list_agents().unwrap()[0].title.clone();
        assert_eq!(title.as_deref(), Some("修复登录 bug"));
        // …and a fresh title replaces it.
        bus.register_with_title("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[], Some("新标题"))
            .unwrap();
        let title = bus.list_agents().unwrap()[0].title.clone();
        assert_eq!(title.as_deref(), Some("新标题"));
        // New registrations without a title stay untitled.
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        assert!(bus.list_agents().unwrap()[1].title.is_none());
    }

    #[test]
    fn list_children_filters_by_parent() {
        let (_dir, bus) = test_bus();
        bus.register_with_profile(
            "ses-00000000000000000000000000000005",
            &[],
            Some("p"),
            None,
            None,
        )
        .unwrap();
        bus.register_with_profile(
            "ses-c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1",
            &[],
            Some("c1"),
            Some("worker"),
            Some("ses-00000000000000000000000000000005"),
        )
        .unwrap();
        bus.register_with_profile(
            "ses-c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2",
            &[],
            Some("c2"),
            None,
            Some("ses-00000000000000000000000000000005"),
        )
        .unwrap();
        bus.register_with_profile(
            "ses-00000000000000000000000000000004",
            &[],
            None,
            None,
            Some("ses-00000000000000000000000000000002"),
        )
        .unwrap();
        let kids = bus
            .list_children("ses-00000000000000000000000000000005")
            .unwrap();
        let mut names: Vec<_> = kids.into_iter().map(|a| a.session_id).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "ses-c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1",
                "ses-c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2"
            ]
        );
    }

    #[test]
    fn list_descendants_bfs_and_cycle_safe() {
        let (_dir, bus) = test_bus();
        bus.register_with_profile(
            "ses-00000000000000000000000000000006",
            &[],
            None,
            None,
            None,
        )
        .unwrap();
        bus.register_with_profile(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &[],
            None,
            None,
            Some("ses-00000000000000000000000000000006"),
        )
        .unwrap();
        bus.register_with_profile(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &[],
            None,
            None,
            Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        )
        .unwrap();
        bus.register_with_profile(
            "ses-cccccccccccccccccccccccccccccccc",
            &[],
            None,
            None,
            Some("ses-00000000000000000000000000000006"),
        )
        .unwrap();
        let mut names = bus
            .list_descendants("ses-00000000000000000000000000000006")
            .unwrap();
        names.sort();
        assert_eq!(
            names,
            vec![
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "ses-cccccccccccccccccccccccccccccccc"
            ]
        );
    }

    #[test]
    fn register_preserves_capabilities_role_parent_on_empty_heartbeat() {
        let (_dir, bus) = test_bus();
        bus.register_with_profile(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &["code".into(), "review".into()],
            Some("worker"),
            Some("coder"),
            Some("ses-00000000000000000000000000000005"),
        )
        .unwrap();
        // Heartbeat with empty capabilities must not wipe the profile.
        bus.register_with_title("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[], Some("worker"))
            .unwrap();
        let info = bus.list_agents().unwrap().into_iter().next().unwrap();
        assert_eq!(info.capabilities, vec!["code", "review"]);
        assert_eq!(info.role.as_deref(), Some("coder"));
        assert_eq!(
            info.parent_session_id.as_deref(),
            Some("ses-00000000000000000000000000000005")
        );
        assert_eq!(info.title.as_deref(), Some("worker"));
        // Non-empty capabilities replace.
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &["search".into()])
            .unwrap();
        let info = bus.list_agents().unwrap().into_iter().next().unwrap();
        assert_eq!(info.capabilities, vec!["search"]);
        assert_eq!(info.role.as_deref(), Some("coder"));
    }

    #[test]
    fn take_matching_replies_extracts_only_target_and_leaves_others() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let mut request = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "please do X",
        );
        request.r#type = MessageType::Request;
        bus.deliver("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &request)
            .unwrap();
        let unrelated = env_from(
            "ses-cccccccccccccccccccccccccccccccc",
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "noise",
        );
        bus.deliver("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &unrelated)
            .unwrap();
        let mut reply = env_from(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "done X",
        );
        reply.r#type = MessageType::Reply;
        reply.in_reply_to_message_id = Some(request.message_id.clone());
        bus.deliver("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &reply)
            .unwrap();
        let mut receipt = Envelope::new(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "已读",
        );
        receipt.r#type = MessageType::Receipt;
        receipt.in_reply_to_message_id = Some(request.message_id.clone());
        bus.deliver("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &receipt)
            .unwrap();

        let got = bus
            .take_matching_replies(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &request.message_id,
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].message_id, reply.message_id);
        assert_eq!(got[0].text, "done X");

        // Unrelated message + receipt remain for the normal inbox drain.
        let left = claim_and_ack(&bus, "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(left.len(), 2);
        assert!(left.iter().any(|e| e.message_id == unrelated.message_id));
        assert!(left.iter().any(|e| e.r#type == MessageType::Receipt));
    }

    #[test]
    fn take_matching_replies_keeps_mailbox_when_archive_append_fails() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let mut request = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "please do X",
        );
        request.r#type = MessageType::Request;
        let mut reply = env_from(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "done X",
        );
        reply.r#type = MessageType::Reply;
        reply.in_reply_to_message_id = Some(request.message_id.clone());
        bus.deliver("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &reply)
            .unwrap();
        let mailbox = bus.mailbox("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let original = std::fs::read_to_string(&mailbox).unwrap();
        bus.fail_archive_append
            .store(true, std::sync::atomic::Ordering::SeqCst);

        assert!(
            bus.take_matching_replies(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &request.message_id,
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(&mailbox).unwrap(), original);

        let recovered = bus
            .take_matching_replies(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &request.message_id,
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].message_id, reply.message_id);
    }

    #[test]
    fn mailbox_temp_recovery_keeps_old_mailbox_or_promotes_complete_replacement() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        let old = env_from(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "old mailbox",
        );
        let replacement = env_from(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "replacement mailbox",
        );
        write_mailbox(
            &bus,
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            std::slice::from_ref(&old),
        );
        let mailbox = bus.mailbox("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let tmp = bus.mailbox_tmp("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        std::fs::write(
            &tmp,
            format!("{}\n", serde_json::to_string(&replacement).unwrap()),
        )
        .unwrap();

        let history = bus
            .history("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 10)
            .unwrap();
        assert!(
            history
                .iter()
                .any(|envelope| envelope.message_id == old.message_id)
        );
        assert!(
            !history
                .iter()
                .any(|envelope| envelope.message_id == replacement.message_id)
        );
        assert!(!tmp.exists());

        std::fs::remove_file(&mailbox).unwrap();
        std::fs::write(
            &tmp,
            format!("{}\n", serde_json::to_string(&replacement).unwrap()),
        )
        .unwrap();
        let history = bus
            .history("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 10)
            .unwrap();
        assert!(
            history
                .iter()
                .any(|envelope| envelope.message_id == replacement.message_id)
        );
        assert!(mailbox.exists());
        assert!(!tmp.exists());
    }

    #[test]
    fn take_matching_replies_rejects_forged_sender() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        bus.register("ses-ffffffffffffffffffffffffffffffff", &[])
            .unwrap();
        let mut request = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "please do X",
        );
        request.r#type = MessageType::Request;
        let mut forged = env_from(
            "ses-ffffffffffffffffffffffffffffffff",
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "I am B",
        );
        forged.r#type = MessageType::Reply;
        forged.in_reply_to_message_id = Some(request.message_id.clone());
        bus.deliver("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &forged)
            .unwrap();
        assert!(
            bus.take_matching_replies(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &request.message_id,
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            )
            .unwrap()
            .is_empty(),
            "forged sender must not satisfy the wait"
        );
        assert_eq!(
            claim_and_ack(&bus, "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").len(),
            1
        );
    }

    #[tokio::test]
    async fn notifier_fires_on_deliver() {
        let (_dir, bus) = test_bus();
        bus.register("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[])
            .unwrap();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let mut rx = bus.subscribe();
        assert!(!rx.has_changed().unwrap_or(false), "no deliveries yet");
        bus.deliver(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &env_from(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "hi",
            ),
        )
        .unwrap();
        assert!(
            rx.has_changed().unwrap_or(false),
            "deliver must bump the in-process notifier"
        );
        let _ = rx.borrow_and_update();
        bus.deliver(
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &env_from(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "hi2",
            ),
        )
        .unwrap();
        assert!(rx.has_changed().unwrap_or(false), "every deliver notifies");
    }

    #[test]
    fn history_returns_newest_first_across_archive_and_mailbox() {
        let (_dir, bus) = test_bus();
        bus.register("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[])
            .unwrap();
        let old = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "已读的旧消息",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&old),
        );
        std::fs::rename(
            bus.mailbox("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            bus.processing("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )
        .unwrap();
        claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let fresh = env_from(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "未读的新消息",
        );
        write_mailbox(
            &bus,
            "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            std::slice::from_ref(&fresh),
        );

        let history = bus
            .history("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 10)
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[0].message_id, fresh.message_id,
            "mailbox (newer) comes first"
        );
        assert_eq!(
            history[1].message_id, old.message_id,
            "archive (older) comes last"
        );
        // The read-only view must not consume the mailbox.
        assert_eq!(
            claim_and_ack(&bus, "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").len(),
            1
        );

        let capped = bus
            .history("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 1)
            .unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].message_id, fresh.message_id);
    }

    #[test]
    fn deletion_tombstone_blocks_resurrection_and_preserves_only_lineage() {
        let (dir, bus) = test_bus();
        let parent = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let deleted = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let descendant = "ses-cccccccccccccccccccccccccccccccc";
        bus.register(parent, &[]).unwrap();
        bus.register_with_profile(
            deleted,
            &["search".into()],
            Some("private title"),
            Some("private role"),
            Some(parent),
        )
        .unwrap();
        bus.register_with_profile(descendant, &[], None, None, Some(deleted))
            .unwrap();

        let acknowledged = env_from(parent, deleted, "already acknowledged");
        bus.deliver(deleted, &acknowledged).unwrap();
        assert_eq!(claim_and_ack(&bus, deleted).len(), 1);

        let pending = env_from(parent, deleted, "still unread");
        bus.deliver(deleted, &pending).unwrap();
        assert_eq!(bus.claim_and_archive(deleted).unwrap().len(), 1);
        assert!(bus.processing(deleted).exists());
        assert!(bus.backlog(deleted).exists());
        std::fs::write(
            bus.backlog_state_tmp(deleted),
            std::fs::read(bus.backlog_state(deleted)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            bus.archive_tmp(deleted),
            serde_json::to_string(&pending).unwrap(),
        )
        .unwrap();

        bus.delete_sessions(&[deleted.to_string()]).unwrap();

        let tombstones = bus.read_deleted_sessions_unlocked().unwrap();
        assert_eq!(tombstones.len(), 1);
        assert_eq!(
            tombstones[deleted].parent_session_id.as_deref(),
            Some(parent)
        );
        let serialized = std::fs::read_to_string(bus.deleted_sessions()).unwrap();
        assert!(!serialized.contains("private title"));
        assert!(!serialized.contains("private role"));
        assert!(
            bus.list_agents()
                .unwrap()
                .iter()
                .all(|agent| agent.session_id != deleted)
        );
        assert!(bus.list_children(parent).unwrap().is_empty());
        assert_eq!(bus.count_registered_children(parent).unwrap(), 1);
        assert_eq!(
            bus.list_descendants(parent).unwrap(),
            vec![descendant.to_string()]
        );

        for path in [
            bus.mailbox(deleted),
            bus.mailbox_tmp(deleted),
            bus.processing(deleted),
            bus.processing_tmp(deleted),
            bus.backlog(deleted),
            bus.backlog_state(deleted),
            bus.backlog_state_tmp(deleted),
            bus.archive(deleted),
            bus.archive_tmp(deleted),
        ] {
            assert!(
                !path.exists(),
                "session message file must be removed: {}",
                path.display()
            );
        }
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            if let Ok(contents) = std::fs::read_to_string(entry.path()) {
                assert!(!contents.contains("already acknowledged"));
                assert!(!contents.contains("still unread"));
            }
        }

        assert!(bus.register(deleted, &[]).is_err());
        assert!(
            bus.deliver(deleted, &env_from(parent, deleted, "late delivery"))
                .is_err()
        );
        assert!(
            bus.deliver(parent, &env_from(deleted, parent, "late sender"))
                .is_err()
        );
        assert!(bus.claim_and_archive(deleted).is_err());
        assert!(
            bus.try_claim_and_archive(deleted)
                .unwrap_err()
                .to_string()
                .contains("deleted")
        );
        assert!(
            bus.ack_claimed(deleted, std::slice::from_ref(&pending.message_id))
                .is_err()
        );
        assert!(bus.last_received(deleted).is_err());
        assert!(bus.find_message(deleted, &acknowledged.message_id).is_err());
        assert!(
            bus.take_matching_replies(deleted, &acknowledged.message_id, parent)
                .is_err()
        );
        assert!(bus.history(deleted, 10).is_err());
        assert!(
            !bus.mailbox(deleted).exists(),
            "stale reads/acks must not recreate mailbox"
        );

        bus.delete_sessions(&[deleted.to_string()]).unwrap();
        assert_eq!(bus.count_registered_children(parent).unwrap(), 1);
    }

    #[test]
    fn concurrent_delivery_and_deletion_are_linearized_by_the_shared_lock() {
        let (dir, bus) = test_bus();
        let sender = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let recipient = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(sender, &[]).unwrap();
        bus.register(recipient, &[]).unwrap();

        let bus = Arc::new(bus);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let delivery_bus = bus.clone();
        let delivery_barrier = barrier.clone();
        let delivery = std::thread::spawn(move || {
            delivery_barrier.wait();
            delivery_bus.deliver(recipient, &env_from(sender, recipient, "racing delivery"))
        });
        let deletion_bus = bus.clone();
        let deletion_barrier = barrier.clone();
        let deletion = std::thread::spawn(move || {
            deletion_barrier.wait();
            deletion_bus.delete_sessions(&[recipient.to_string()])
        });
        barrier.wait();

        let _ = delivery.join().unwrap();
        deletion.join().unwrap().unwrap();
        assert!(!dir.path().join(format!("{recipient}.jsonl")).exists());
        assert!(bus.history(recipient, 10).is_err());
        assert!(
            bus.register(recipient, &[]).is_err(),
            "deletion must win permanently over a racing delivery"
        );
    }

    #[test]
    fn interrupted_tombstone_replacement_recovers_before_registration() {
        let (_dir, bus) = test_bus();
        let deleted = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let tombstone = DeletedSessionEntry {
            parent_session_id: Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
        };
        std::fs::write(
            bus.deleted_sessions_tmp(),
            serde_json::to_string(&HashMap::from([(deleted.to_string(), tombstone)])).unwrap(),
        )
        .unwrap();

        assert!(bus.register(deleted, &[]).is_err());
        assert!(bus.deleted_sessions().exists());
        assert!(!bus.deleted_sessions_tmp().exists());
        assert!(!bus.mailbox(deleted).exists());
    }

    #[test]
    fn invalid_initial_tombstone_temp_is_discarded_before_retry() {
        let (_dir, bus) = test_bus();
        let session_id = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(session_id, &[]).unwrap();
        std::fs::write(
            bus.deleted_sessions_tmp(),
            format!("{{\"{session_id}\":{{\"parent_session_id\":"),
        )
        .unwrap();

        // A partial first write was never committed. It must not poison all
        // future reads or prevent the still-existing session from retrying.
        bus.delete_sessions(&[session_id.to_string()]).unwrap();

        assert!(!bus.deleted_sessions_tmp().exists());
        assert!(bus.deleted_sessions().exists());
        assert!(
            bus.read_deleted_sessions_unlocked()
                .unwrap()
                .contains_key(session_id)
        );
        assert!(!bus.mailbox(session_id).exists());
    }

    #[test]
    fn invalid_replacement_temp_restores_previous_tombstones_before_retry() {
        let (_dir, bus) = test_bus();
        let previously_deleted = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let retry_session = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        bus.register(retry_session, &[]).unwrap();
        bus.write_deleted_sessions_unlocked(&HashMap::from([(
            previously_deleted.to_string(),
            DeletedSessionEntry {
                parent_session_id: None,
            },
        )]))
        .unwrap();

        // Model interruption after the old canonical file was moved aside,
        // followed by a damaged replacement temp.
        std::fs::rename(bus.deleted_sessions(), bus.deleted_sessions_backup()).unwrap();
        std::fs::write(
            bus.deleted_sessions_tmp(),
            format!("{{\"{retry_session}\":{{\"parent_session_id\":"),
        )
        .unwrap();

        bus.delete_sessions(&[retry_session.to_string()]).unwrap();

        let tombstones = bus.read_deleted_sessions_unlocked().unwrap();
        assert!(tombstones.contains_key(previously_deleted));
        assert!(tombstones.contains_key(retry_session));
        assert!(!bus.deleted_sessions_tmp().exists());
        assert!(!bus.deleted_sessions_backup().exists());
        assert!(!bus.mailbox(retry_session).exists());
    }

    #[test]
    fn deleted_session_lineage_byte_budget_accepts_limit_and_rejects_overflow() {
        assert!(validate_deleted_sessions_size(MAX_DELETED_SESSIONS_BYTES).is_ok());
        assert!(validate_deleted_sessions_size(MAX_DELETED_SESSIONS_BYTES + 1).is_err());
    }

    #[test]
    fn delete_all_also_tombstones_legacy_unregistered_mailboxes() {
        let (_dir, bus) = test_bus();
        let legacy = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let database_only = "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let sidecar_only = "ses-cccccccccccccccccccccccccccccccc";
        bus.register(legacy, &[]).unwrap();
        bus.unregister(legacy).unwrap();
        write_mailbox(
            &bus,
            legacy,
            &[env_from(database_only, legacy, "legacy pending")],
        );
        std::fs::write(bus.backlog_state_tmp(sidecar_only), "orphan state").unwrap();

        bus.delete_all_sessions(&[database_only.to_string()])
            .unwrap();

        assert!(!bus.mailbox(legacy).exists());
        assert!(bus.history(legacy, 10).is_err());
        assert!(bus.register(legacy, &[]).is_err());
        assert!(bus.register(database_only, &[]).is_err());
        assert!(bus.register(sidecar_only, &[]).is_err());
        assert!(
            bus.read_deleted_sessions_unlocked()
                .unwrap()
                .contains_key(legacy)
        );
        assert!(
            bus.read_deleted_sessions_unlocked()
                .unwrap()
                .contains_key(sidecar_only)
        );
        assert!(!bus.backlog_state_tmp(sidecar_only).exists());
    }
}
