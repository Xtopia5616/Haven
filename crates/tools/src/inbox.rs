//! Cross-session agent messaging: a lightweight file-based message bus that
//! lets independent agent sessions exchange messages like human colleagues.
//!
//! Zero external dependencies: one shared directory (see
//! [`default_inbox_dir`], `<data_dir>/inbox`) with a registry file and one
//! JSONL mailbox per agent.
//!
//! Layout:
//! - `agents.json` — registry: agent name → metadata (`last_seen` heartbeat)
//! - `<name>.jsonl` — per-agent mailbox (append-only, one JSON envelope per line)
//! - `<name>.archive.jsonl` — read messages kept for audit
//! - `.lock` — cross-process file mutex (Windows-safe)
//!
//! Concurrency model: every mutation (append, read-and-archive, registry
//! update) runs under the `.lock` mutex; registry updates additionally write a
//! temp file and atomically rename it. Inbox reads are "read then move": the
//! mailbox is renamed to `.processing` under the lock, so a concurrent append
//! can never tear a read, and a message is never read twice — even across
//! process crashes, a leftover `.processing` file is drained on the next call
//! (crash recovery, with id-deduplication against the archive tail).
//!
//! Envelopes are single-line JSON per the interop format; ids are canonical
//! `msg-{uuid32}` ([`haven_common::types::new_id`]). Agent names double as
//! mailbox filenames and are strictly validated (`^[A-Za-z0-9_-]{1,64}$`) to
//! prevent path traversal and name collisions (`.` is excluded so `<name>`
//! can never clash with `agents.json` / `*.archive.jsonl`).

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, SecondsFormat};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use haven_common::types::new_id;

/// Heartbeat threshold: agents whose `last_seen` is older than this are
/// reported `offline` by [`InboxBus::list_agents`] (entries are never
/// deleted — history is kept).
pub const OFFLINE_AFTER: Duration = Duration::from_secs(300);

/// Wait between lock-file acquisition retries.
const LOCK_WAIT: Duration = Duration::from_millis(20);
/// Upper bound for waiting on a held lock before failing the operation.
/// Deliberately LONGER than [`LOCK_STALE_AFTER`] so a crashed holder's stale
/// lock is broken within a single call's wait instead of erroring until the
/// lock ages past the stale threshold.
const LOCK_TIMEOUT: Duration = Duration::from_secs(15);
/// A lock file older than this is considered stale (crashed holder) and is
/// broken.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(10);
/// Archive tail scanned for duplicate ids when archiving (crash recovery:
/// a message archived but not deleted from `.processing` must not be
/// archived twice).
const ARCHIVE_DEDUP_TAIL_BYTES: u64 = 64 * 1024;

/// Default bus root: `<data_dir>/inbox` (`%APPDATA%/haven/inbox` on Windows).
pub fn default_inbox_dir() -> PathBuf {
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
pub struct InboxNotifier {
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

/// Validate an agent name: `^[A-Za-z0-9_-]{1,64}$`. The name becomes a
/// mailbox filename, so anything else (path separators, dots for
/// `.archive`/`agents.json` collisions, control characters) is rejected.
pub fn validate_agent_name(name: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        anyhow::bail!("invalid agent name '{name}': only [a-zA-Z0-9_-], max 64 chars")
    }
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
/// Required fields: `id` / `from` / `to` / `text` / `created_at`; everything
/// else is optional and serialized as `null` when absent (kept on the wire
/// for audit readability, matching the interop format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Unique message id, canonical `msg-{uuid32}`.
    pub id: String,
    /// `message | reply | broadcast | request | system`.
    #[serde(default)]
    pub r#type: MessageType,
    /// Sender agent name.
    pub from: String,
    /// Recipient agent name, or `*` for broadcast.
    pub to: String,
    /// Address replies should go to; defaults to `from` when absent.
    pub reply_address: Option<String>,
    /// When this envelope is a reply: the id of the original message.
    pub in_reply_to: Option<String>,
    /// Optional conversation-thread id for grouping multi-turn exchanges
    /// (reserved: not exposed by the current tools, kept for interop).
    pub thread_id: Option<String>,
    pub subject: Option<String>,
    pub text: String,
    /// Optional structured payload (JSON only; files referenced by path).
    pub payload: Option<serde_json::Value>,
    /// RFC3339 creation time.
    pub created_at: String,
    /// Optional RFC3339 expiry: expired messages are not returned by
    /// [`InboxBus::read_and_archive`] (still archived for audit).
    pub expires_at: Option<String>,
}

impl Envelope {
    pub fn new(from: &str, to: &str, text: &str) -> Self {
        Self {
            id: new_id("msg"),
            r#type: MessageType::Message,
            from: from.into(),
            to: to.into(),
            reply_address: None,
            in_reply_to: None,
            thread_id: None,
            subject: None,
            text: text.into(),
            payload: None,
            created_at: now_rfc3339(),
            expires_at: None,
        }
    }

    /// The address a reply to this message should target: its custom
    /// `reply_address` when set, else its sender.
    pub fn reply_target(&self) -> &str {
        self.reply_address.as_deref().unwrap_or(&self.from)
    }
}

/// Registry entry for one agent (`agents.json` value).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    /// RFC3339 heartbeat timestamp.
    pub last_seen: String,
    /// RFC3339 first-registration timestamp.
    pub started_at: String,
    /// Optional human-readable session title (shown by agents_list/UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
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
    pub name: String,
    pub last_seen: String,
    pub started_at: String,
    pub status: AgentStatus,
    pub title: Option<String>,
    pub capabilities: Vec<String>,
}

/// Result of delivering one envelope to one recipient.
#[derive(Debug, Clone, Serialize)]
pub struct SendOutcome {
    pub to: String,
    /// Whether the envelope was appended to the recipient's mailbox.
    /// Lenient delivery: `true` even when the recipient's heartbeat is stale
    /// (they read it next time they poll); only agents that never registered
    /// (no mailbox) are rejected up front.
    pub delivered: bool,
    /// Recipient liveness at delivery time.
    pub status: AgentStatus,
}

/// The shared message bus. All operations are synchronous file I/O under the
/// `.lock` mutex; async callers should wrap them in `spawn_blocking`.
#[derive(Debug, Clone)]
pub struct InboxBus {
    root: PathBuf,
    notifier: Arc<InboxNotifier>,
}

impl InboxBus {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let (notifier, _rx) = InboxNotifier::new();
        Self {
            root: root.into(),
            notifier: Arc::new(notifier),
        }
    }

    /// Bus rooted at `<data_dir>/inbox`, sharing the process-wide notifier so
    /// delivery notifications reach every in-process consumer.
    pub fn default_root() -> Self {
        Self {
            root: default_inbox_dir(),
            notifier: shared_notifier(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Subscribe to delivery notifications for this bus.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.notifier.subscribe()
    }

    fn mailbox(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.jsonl"))
    }

    fn archive(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.archive.jsonl"))
    }

    fn processing(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.jsonl.processing"))
    }

    fn ensure_dir(&self) -> anyhow::Result<()> {
        Ok(std::fs::create_dir_all(&self.root)?)
    }

    /// Register (or re-register) the given agent: upsert its registry entry
    /// (heartbeat = "now") and make sure its mailbox file exists. Every tool
    /// call starts with this, so no separate heartbeat mechanism is needed.
    pub fn register(&self, name: &str, capabilities: &[String]) -> anyhow::Result<()> {
        self.register_with_title(name, capabilities, None)
    }

    /// Like [`InboxBus::register`], but with an optional human-readable
    /// session title for the registry (used by the ReAct loop's heartbeat so
    /// `agents_list` and the UI can show what a session is about).
    pub fn register_with_title(
        &self,
        name: &str,
        capabilities: &[String],
        title: Option<&str>,
    ) -> anyhow::Result<()> {
        validate_agent_name(name)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let now = now_rfc3339();
        let mut reg = self.read_registry_unlocked()?;
        match reg.get_mut(name) {
            Some(e) => {
                e.last_seen = now.clone();
                if title.is_some() {
                    e.title = title.map(String::from);
                }
                e.capabilities = capabilities.to_vec();
            }
            None => {
                reg.insert(
                    name.into(),
                    AgentEntry {
                        name: name.into(),
                        last_seen: now.clone(),
                        started_at: now,
                        title: title.map(String::from),
                        capabilities: capabilities.to_vec(),
                    },
                );
            }
        }
        self.write_registry_unlocked(&reg)?;
        // Create the mailbox if missing (the existence of this file is what
        // message_send checks to decide "agent exists").
        let mailbox = self.mailbox(name);
        if !mailbox.exists() {
            OpenOptions::new().create(true).append(true).open(mailbox)?;
        }
        Ok(())
    }

    /// Graceful shutdown: remove the agent from the registry. The mailbox and
    /// archive are kept, so late messages remain deliverable (reported as
    /// `offline`) and the history survives a later re-registration. Missing
    /// entries are a no-op.
    pub fn unregister(&self, name: &str) -> anyhow::Result<()> {
        validate_agent_name(name)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let mut reg = self.read_registry_unlocked()?;
        if reg.remove(name).is_some() {
            self.write_registry_unlocked(&reg)?;
        }
        Ok(())
    }

    /// All registered agents with computed liveness, online first.
    pub fn list_agents(&self) -> anyhow::Result<Vec<AgentInfo>> {
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let now = Local::now();
        let mut out: Vec<AgentInfo> = self
            .read_registry_unlocked()?
            .into_values()
            .map(|e| AgentInfo {
                status: if is_online(&e.last_seen, now) {
                    AgentStatus::Online
                } else {
                    AgentStatus::Offline
                },
                name: e.name,
                last_seen: e.last_seen,
                started_at: e.started_at,
                title: e.title,
                capabilities: e.capabilities,
            })
            .collect();
        out.sort_by(|a, b| {
            let ka = (a.status == AgentStatus::Offline, &a.name);
            let kb = (b.status == AgentStatus::Offline, &b.name);
            ka.cmp(&kb)
        });
        Ok(out)
    }

    /// Append one envelope to the recipient's mailbox. Lenient delivery: a
    /// registered-but-stale recipient still receives the message (its
    /// heartbeat freshness is only reported in [`SendOutcome::status`]);
    /// only a missing mailbox (never registered) is an error.
    pub fn deliver(&self, to: &str, env: &Envelope) -> anyhow::Result<SendOutcome> {
        validate_agent_name(to)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
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
        let mut f = OpenOptions::new().append(true).open(&mailbox)?;
        f.write_all(line.as_bytes())?;
        f.flush()?;
        // In-process wake-up: the recipient's ReAct loop can react to the
        // notification instead of waiting for its next polling step.
        self.notifier.notify();
        Ok(SendOutcome {
            to: to.into(),
            delivered: true,
            status,
        })
    }

    /// Read-and-archive: atomically drain this agent's mailbox, append the
    /// messages to its archive (deduplicated against the archive tail), and
    /// return the fresh messages. Expired envelopes are archived but not
    /// returned, and envelopes already archived by a crashed earlier attempt
    /// are neither re-archived nor returned. Never returns the same message
    /// twice — even after a crash, because a leftover `.processing` file is
    /// drained first.
    ///
    /// After draining, the empty mailbox is recreated so future sends keep
    /// working (an existing mailbox is what `deliver` treats as "agent
    /// registered").
    pub fn read_and_archive(&self, name: &str) -> anyhow::Result<Vec<Envelope>> {
        validate_agent_name(name)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let mut collected: Vec<Envelope> = Vec::new();
        let pending = self.processing(name);
        loop {
            if !pending.exists() {
                let mailbox = self.mailbox(name);
                if !mailbox.exists() {
                    break;
                }
                std::fs::rename(&mailbox, &pending)?;
            }
            let content = match std::fs::read_to_string(&pending) {
                Ok(c) => c,
                // Crash between rename and open: treat as empty.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e.into()),
            };
            let mut envs: Vec<Envelope> = Vec::new();
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Envelope>(line) {
                    Ok(env) => envs.push(env),
                    Err(e) => {
                        tracing::warn!("inbox: skipping corrupt line in mailbox '{name}': {e}")
                    }
                }
            }
            let archive_ids = self.read_archive_tail_ids(name)?;
            if !envs.is_empty() {
                let mut af = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.archive(name))?;
                for env in &envs {
                    if !archive_ids.contains(&env.id) {
                        writeln!(af, "{}", serde_json::to_string(env)?)?;
                    }
                }
                af.flush()?;
            }
            std::fs::remove_file(&pending)?;
            // The dedup filter applies to the RETURN too: after a crash
            // between archiving and deleting `.processing`, the same envelope
            // must not be delivered to the agent twice.
            collected.extend(
                envs.into_iter()
                    .filter(|e| !archive_ids.contains(&e.id) && !is_expired(e)),
            );
        }
        // Recreate the empty mailbox (under the same lock) so `deliver` to a
        // registered agent keeps working after its inbox was drained.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.mailbox(name))?;
        Ok(collected)
    }

    /// The most recent message this agent received (unread mailbox first,
    /// then archive tail). Used by `message_reply` to resolve a missing `to`.
    pub fn last_received(&self, name: &str) -> anyhow::Result<Option<Envelope>> {
        validate_agent_name(name)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        if let Some(env) = last_valid_line(&self.mailbox(name)) {
            return Ok(Some(env));
        }
        Ok(last_valid_line(&self.archive(name)))
    }

    /// Find one envelope by id in this agent's mailbox or archive. Used by
    /// `message_reply` to resolve the target of a `in_reply_to` reference.
    /// The archive is scanned from the tail first (recent replies dominate),
    /// falling back to a full scan when the id is old.
    pub fn find_message(&self, name: &str, id: &str) -> anyhow::Result<Option<Envelope>> {
        validate_agent_name(name)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        for path in [self.mailbox(name), self.archive(name)] {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if let Some(env) =
                content
                    .lines()
                    .find_map(|l| match serde_json::from_str::<Envelope>(l.trim()) {
                        Ok(e) if e.id == id => Some(e),
                        _ => None,
                    })
            {
                return Ok(Some(env));
            }
        }
        Ok(None)
    }

    /// Auto-ack every freshly read message with a lightweight `receipt`
    /// envelope back to its reply target, so the sender learns the message
    /// was actually read. Receipts are never acked themselves, and messages
    /// from ourself get no ack. Best-effort: a failed delivery (recipient
    /// unregistered) is logged and skipped. Shared by the `message_inbox`
    /// tool and the ReAct loop's automatic inbox check.
    pub fn send_receipts(&self, name: &str, read: &[Envelope]) -> Vec<SendOutcome> {
        let mut outcomes = Vec::new();
        for env in read {
            if env.r#type == MessageType::Receipt || env.from == name {
                continue;
            }
            let to = env.reply_target();
            let mut receipt = Envelope::new(name, to, "已读");
            receipt.r#type = MessageType::Receipt;
            receipt.in_reply_to = Some(env.id.clone());
            receipt.reply_address = Some(name.into());
            match self.deliver(to, &receipt) {
                Ok(o) => outcomes.push(o),
                Err(e) => tracing::debug!("inbox: receipt to '{to}' failed: {e}"),
            }
        }
        outcomes
    }

    /// Message history of one agent: unread mailbox messages plus the
    /// read archive, newest first, up to `limit` entries. Read-only view for
    /// the UI / audit (never consumes the mailbox).
    pub fn history(&self, name: &str, limit: usize) -> anyhow::Result<Vec<Envelope>> {
        validate_agent_name(name)?;
        let _lock = LockGuard::acquire(&self.root)?;
        self.ensure_dir()?;
        let mut entries: Vec<Envelope> = Vec::new();
        // Archive (older) first, then the unread mailbox (newer), so the
        // reversal below yields strictly newest-first across both files.
        for path in [self.archive(name), self.mailbox(name)] {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    if let Ok(env) = serde_json::from_str::<Envelope>(line.trim()) {
                        entries.push(env);
                    }
                }
            }
        }
        // Mailbox lines are newer than archive lines, so reverse order gives
        // newest first within each file; files are already in age order.
        entries.reverse();
        entries.truncate(limit);
        Ok(entries)
    }

    fn read_registry_unlocked(&self) -> anyhow::Result<HashMap<String, AgentEntry>> {
        let path = self.root.join("agents.json");
        match std::fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => Ok(HashMap::new()),
            Ok(s) => {
                serde_json::from_str(&s).map_err(|e| anyhow::anyhow!("corrupt agents.json: {e}"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomic registry update: write a temp file, then rename over
    /// `agents.json` (caller must hold the lock).
    pub(crate) fn write_registry_unlocked(
        &self,
        reg: &HashMap<String, AgentEntry>,
    ) -> anyhow::Result<()> {
        let tmp = self.root.join("agents.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(reg)?)?;
        std::fs::rename(&tmp, self.root.join("agents.json"))?;
        Ok(())
    }

    /// Ids of the last messages in the archive, for append deduplication
    /// (crash recovery only; bounded scan keeps this O(1)-ish in practice).
    fn read_archive_tail_ids(&self, name: &str) -> anyhow::Result<HashSet<String>> {
        Ok(read_tail(&self.archive(name), ARCHIVE_DEDUP_TAIL_BYTES)
            .lines()
            .filter_map(|l| serde_json::from_str::<Envelope>(l.trim()).ok())
            .map(|e| e.id)
            .collect())
    }
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

/// Expired when `expires_at` is set, parses, and is in the past. Unparseable
/// or absent `expires_at` is treated as not expired.
fn is_expired(env: &Envelope) -> bool {
    env.expires_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|ts| {
            let now = Local::now();
            now.signed_duration_since(ts.with_timezone(&now.timezone())) > chrono::Duration::zero()
        })
}

/// Read at most the last `max_bytes` of a file as UTF-8 (missing/unreadable
/// files yield an empty string). Used for tail scans so archive size never
/// degrades reply resolution or dedup lookups.
fn read_tail(path: &Path, max_bytes: u64) -> String {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    let _ = f.seek(SeekFrom::Start(start));
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s
}

fn last_valid_line(path: &Path) -> Option<Envelope> {
    read_tail(path, ARCHIVE_DEDUP_TAIL_BYTES)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Envelope>(l.trim()).ok())
}

/// Cross-process file mutex. Acquisition is atomic (`create_new`); stale
/// locks (crashed holder, mtime older than [`LOCK_STALE_AFTER`]) are broken;
/// release removes the file only if it still carries our pid so a broken
/// lock held by someone else is never deleted.
struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    fn acquire(root: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(root)?;
        let path = root.join(".lock");
        let deadline = SystemTime::now() + LOCK_TIMEOUT;
        let pid = std::process::id();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = writeln!(f, "pid={pid}");
                    return Ok(Self { path });
                }
                // Windows quirk: a `create_new` racing with another thread's
                // release-delete can hit ERROR_ACCESS_DENIED (delete-pending)
                // instead of ERROR_FILE_EXISTS — both mean "lock busy, retry".
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    if lock_is_stale(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if SystemTime::now() >= deadline {
                        anyhow::bail!("inbox lock timed out: another agent holds {path:?}");
                    }
                    std::thread::sleep(LOCK_WAIT);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn lock_is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .is_some_and(|t| t.elapsed().unwrap_or_default() > LOCK_STALE_AFTER)
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let mine = std::fs::read_to_string(&self.path).unwrap_or_default();
        if mine.trim() == format!("pid={}", std::process::id()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_bus() -> (tempfile::TempDir, InboxBus) {
        let dir = tempfile::tempdir().unwrap();
        let bus = InboxBus::new(dir.path());
        (dir, bus)
    }

    use std::sync::Arc;

    fn env_from(a: &str, to: &str, text: &str) -> Envelope {
        Envelope::new(a, to, text)
    }

    fn write_mailbox(bus: &InboxBus, name: &str, envs: &[Envelope]) {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(bus.mailbox(name))
            .unwrap();
        for e in envs {
            writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
        }
    }

    fn reg_entry(name: &str, last_seen: &str) -> AgentEntry {
        AgentEntry {
            name: name.into(),
            last_seen: last_seen.into(),
            started_at: "2026-01-01T00:00:00+08:00".into(),
            title: None,
            capabilities: vec![],
        }
    }

    #[test]
    fn validate_agent_name_accepts_safe_names() {
        for name in ["ses-abc123", "agent-alpha", "A0_-x", "ses-0123456789abcdef"] {
            assert!(validate_agent_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn validate_agent_name_rejects_unsafe_names() {
        for name in [
            "",
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
            assert!(validate_agent_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn register_creates_mailbox_and_registry_entry() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &["x".into()]).unwrap();
        assert!(bus.mailbox("ses-a").exists());
        let reg: HashMap<String, AgentEntry> =
            serde_json::from_str(&std::fs::read_to_string(bus.root().join("agents.json")).unwrap())
                .unwrap();
        assert_eq!(reg["ses-a"].capabilities, vec!["x"]);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn register_second_call_acts_as_heartbeat_without_duplicate() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        let first_seen = {
            let reg = serde_json::from_str::<HashMap<String, AgentEntry>>(
                &std::fs::read_to_string(bus.root().join("agents.json")).unwrap(),
            )
            .unwrap();
            reg["ses-a"].last_seen.clone()
        };
        bus.register("ses-a", &[]).unwrap();
        let reg = serde_json::from_str::<HashMap<String, AgentEntry>>(
            &std::fs::read_to_string(bus.root().join("agents.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(reg.len(), 1, "re-register must not duplicate the entry");
        assert!(reg["ses-a"].last_seen >= first_seen);
    }

    #[test]
    fn register_rejects_invalid_name_without_creating_files() {
        let (_dir, bus) = test_bus();
        assert!(bus.register("../evil", &[]).is_err());
        assert!(!bus.mailbox("../evil").exists());
    }

    #[test]
    fn send_and_read_roundtrip() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        bus.register("ses-b", &[]).unwrap();
        let mut env = env_from("ses-a", "ses-b", "schema 定了吗？");
        env.subject = Some("需要你确认".into());
        env.payload = Some(json!({"file": "/path/x.rs"}));
        let outcome = bus.deliver("ses-b", &env).unwrap();
        assert!(outcome.delivered);
        assert_eq!(outcome.status, AgentStatus::Online);

        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(msgs.len(), 1);
        let got = &msgs[0];
        assert_eq!(got.id, env.id);
        assert_eq!(got.from, "ses-a");
        assert_eq!(got.to, "ses-b");
        assert_eq!(got.text, "schema 定了吗？");
        assert_eq!(got.subject.as_deref(), Some("需要你确认"));
        assert_eq!(got.payload, Some(json!({"file": "/path/x.rs"})));
        assert!(
            DateTime::parse_from_rfc3339(&got.created_at).is_ok(),
            "created_at must be RFC3339, got {}",
            got.created_at
        );

        // Read-then-move: a second read yields nothing.
        assert!(bus.read_and_archive("ses-b").unwrap().is_empty());
        // The archive keeps the full history for audit.
        let archive = std::fs::read_to_string(bus.archive("ses-b")).unwrap();
        assert_eq!(archive.lines().count(), 1);
        // The drained mailbox is recreated: the agent can still receive.
        let outcome = bus
            .deliver("ses-b", &env_from("ses-a", "ses-b", "第二封"))
            .unwrap();
        assert!(outcome.delivered);
        assert_eq!(bus.read_and_archive("ses-b").unwrap().len(), 1);
    }

    #[test]
    fn send_to_unregistered_agent_errors() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        let err = bus
            .deliver("ses-b", &env_from("ses-a", "ses-b", "hi"))
            .unwrap_err();
        assert!(err.to_string().contains("not found or offline"), "{err}");
    }

    #[test]
    fn send_to_stale_agent_is_lenient_but_reports_offline() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        bus.register("ses-b", &[]).unwrap();
        // Rewrite the registry so ses-b looks stale (heartbeat > 300s ago).
        let old = (Local::now()
            - chrono::Duration::from_std(OFFLINE_AFTER).unwrap()
            - chrono::Duration::seconds(10))
        .to_rfc3339_opts(SecondsFormat::Secs, false);
        let mut reg = HashMap::new();
        reg.insert("ses-b".into(), reg_entry("ses-b", &old));
        bus.write_registry_unlocked(&reg).unwrap();

        let outcome = bus
            .deliver("ses-b", &env_from("ses-a", "ses-b", "hi"))
            .unwrap();
        assert!(
            outcome.delivered,
            "stale agents still receive (lenient delivery)"
        );
        assert_eq!(outcome.status, AgentStatus::Offline);
        // The message is there when ses-b eventually polls.
        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn expired_messages_are_not_returned_but_still_archived() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        bus.register("ses-b", &[]).unwrap();
        let mut env = env_from("ses-a", "ses-b", "过期了");
        env.expires_at = Some(
            (Local::now() - chrono::Duration::minutes(1))
                .to_rfc3339_opts(SecondsFormat::Secs, false),
        );
        bus.deliver("ses-b", &env).unwrap();
        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert!(msgs.is_empty(), "expired messages must be filtered");
        let archive = std::fs::read_to_string(bus.archive("ses-b")).unwrap();
        assert_eq!(archive.lines().count(), 1, "but archived for audit");
    }

    #[test]
    fn future_expiry_is_returned() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        bus.register("ses-b", &[]).unwrap();
        let mut env = env_from("ses-a", "ses-b", "还有效");
        env.expires_at = Some(
            (Local::now() + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, false),
        );
        bus.deliver("ses-b", &env).unwrap();
        assert_eq!(bus.read_and_archive("ses-b").unwrap().len(), 1);
    }

    #[test]
    fn corrupt_lines_are_skipped_without_losing_others() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        let good = env_from("ses-a", "ses-b", "ok");
        write_mailbox(&bus, "ses-b", &[good.clone()]);
        let mut f = OpenOptions::new()
            .append(true)
            .open(bus.mailbox("ses-b"))
            .unwrap();
        writeln!(f, "{{not json").unwrap();
        writeln!(
            f,
            "{}",
            serde_json::to_string(&env_from("ses-a", "ses-b", "ok2")).unwrap()
        )
        .unwrap();
        drop(f);
        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, good.id);
    }

    #[test]
    fn crash_recovery_drains_leftover_processing() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        let env = env_from("ses-a", "ses-b", "crash 前写的");
        write_mailbox(&bus, "ses-b", &[env.clone()]);
        // Simulate a crash after the mailbox → .processing rename.
        std::fs::rename(bus.mailbox("ses-b"), bus.processing("ses-b")).unwrap();
        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(msgs.len(), 1, "leftover .processing must be drained");
        assert!(bus.read_and_archive("ses-b").unwrap().is_empty());
        let archive = std::fs::read_to_string(bus.archive("ses-b")).unwrap();
        assert_eq!(archive.lines().count(), 1);
    }

    #[test]
    fn crash_recovery_with_new_writes_after_crash() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        let first = env_from("ses-a", "ses-b", "第一次");
        write_mailbox(&bus, "ses-b", &[first.clone()]);
        std::fs::rename(bus.mailbox("ses-b"), bus.processing("ses-b")).unwrap();
        // New writes land in the fresh mailbox after the crash.
        write_mailbox(&bus, "ses-b", &[env_from("ses-a", "ses-b", "第二次")]);
        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(
            msgs.len(),
            2,
            "both the leftover and the new writes must be read"
        );
        assert_eq!(msgs[0].id, first.id);
    }

    #[test]
    fn archive_dedupes_after_rearchiving_same_processing() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        let env = env_from("ses-a", "ses-b", "同一封");
        write_mailbox(&bus, "ses-b", &[env.clone()]);
        std::fs::rename(bus.mailbox("ses-b"), bus.processing("ses-b")).unwrap();
        // Crash AFTER archiving but BEFORE deleting .processing: the message
        // is already in the archive. Re-reading must not duplicate it — in
        // the archive NOR in what is returned to the agent.
        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(msgs.len(), 1, "first drain returns the message");
        write_mailbox(&bus, "ses-b", &[env.clone()]);
        std::fs::rename(bus.mailbox("ses-b"), bus.processing("ses-b")).unwrap();
        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert!(
            msgs.is_empty(),
            "a message already archived must not be returned twice"
        );
        let archive = std::fs::read_to_string(bus.archive("ses-b")).unwrap();
        assert_eq!(
            archive.lines().count(),
            1,
            "the same message id must not be archived twice"
        );
    }

    #[test]
    fn concurrent_delivers_produce_no_corrupt_lines() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        let bus = Arc::new(bus);
        let mut handles = Vec::new();
        for t in 0..8 {
            let bus = bus.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..25 {
                    bus.deliver(
                        "ses-b",
                        &env_from(&format!("ses-w{t}"), "ses-b", &format!("m{t}-{i}")),
                    )
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let msgs = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(
            msgs.len(),
            8 * 25,
            "every concurrent write must survive intact"
        );
        let ids: HashSet<String> = msgs.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), msgs.len(), "ids must be unique");
    }

    #[test]
    fn list_agents_reports_online_and_offline() {
        let (_dir, bus) = test_bus();
        bus.register("ses-online", &[]).unwrap();
        bus.register("ses-offline", &[]).unwrap();
        // Age out ses-offline's heartbeat in place (both entries kept).
        let old = (Local::now()
            - chrono::Duration::from_std(OFFLINE_AFTER).unwrap()
            - chrono::Duration::seconds(10))
        .to_rfc3339_opts(SecondsFormat::Secs, false);
        let mut reg = bus.read_registry_unlocked().unwrap();
        reg.get_mut("ses-offline").unwrap().last_seen = old;
        bus.write_registry_unlocked(&reg).unwrap();

        let agents = bus.list_agents().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name, "ses-online", "online agents come first");
        assert_eq!(agents[0].status, AgentStatus::Online);
        assert_eq!(agents[1].status, AgentStatus::Offline);
    }

    #[test]
    fn last_received_prefers_mailbox_then_archive_and_honors_reply_address() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        // Archive history: an old message from ses-a.
        let old = env_from("ses-a", "ses-b", "旧消息");
        write_mailbox(&bus, "ses-b", &[old.clone()]);
        std::fs::rename(bus.mailbox("ses-b"), bus.processing("ses-b")).unwrap();
        bus.read_and_archive("ses-b").unwrap();
        // New unread message from ses-c with a custom reply_address.
        let mut fresh = env_from("ses-c", "ses-b", "新消息");
        fresh.reply_address = Some("ses-cc".into());
        write_mailbox(&bus, "ses-b", &[fresh.clone()]);

        let last = bus.last_received("ses-b").unwrap().unwrap();
        assert_eq!(last.id, fresh.id, "unread mailbox wins over archive");
        assert_eq!(
            last.reply_target(),
            "ses-cc",
            "reply_address overrides from"
        );

        // After reading the mailbox, the archive tail provides the last sender.
        bus.read_and_archive("ses-b").unwrap();
        let last = bus.last_received("ses-b").unwrap().unwrap();
        assert_eq!(last.id, fresh.id);
        assert_eq!(last.reply_target(), "ses-cc");
    }

    #[test]
    fn last_received_empty_when_no_history() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        assert!(bus.last_received("ses-b").unwrap().is_none());
    }

    #[test]
    fn find_message_searches_mailbox_and_archive() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        let archived = env_from("ses-a", "ses-b", "已读");
        write_mailbox(&bus, "ses-b", &[archived.clone()]);
        std::fs::rename(bus.mailbox("ses-b"), bus.processing("ses-b")).unwrap();
        bus.read_and_archive("ses-b").unwrap();
        let fresh = env_from("ses-a", "ses-b", "未读");
        write_mailbox(&bus, "ses-b", &[fresh.clone()]);

        let found = bus.find_message("ses-b", &fresh.id).unwrap().unwrap();
        assert_eq!(found.id, fresh.id);
        let found = bus.find_message("ses-b", &archived.id).unwrap().unwrap();
        assert_eq!(found.id, archived.id);
        assert!(
            bus.find_message("ses-b", "msg-nonexistent")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stale_lock_is_broken() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        let lock_path = bus.root().join(".lock");
        // A crashed holder left a lock older than LOCK_STALE_AFTER behind.
        std::fs::write(&lock_path, "pid=999999").unwrap();
        let file = File::options().write(true).open(&lock_path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(30))
            .unwrap();
        drop(file);
        let guard = LockGuard::acquire(bus.root()).unwrap();
        drop(guard);
        assert!(!lock_path.exists(), "release must remove the lock");
    }

    #[test]
    fn lock_not_released_if_stolen_by_another_pid() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        let lock_path = bus.root().join(".lock");
        // Simulate a stolen/replaced lock: our guard's drop must not delete a
        // lock file that no longer carries our pid.
        let guard = LockGuard::acquire(bus.root()).unwrap();
        std::fs::write(&lock_path, "pid=1").unwrap();
        drop(guard);
        assert!(
            lock_path.exists(),
            "another pid's lock must survive our drop"
        );
        let _ = std::fs::remove_file(&lock_path);
    }

    #[test]
    fn mailbox_paths_follow_the_spec_layout() {
        let (_dir, bus) = test_bus();
        assert_eq!(bus.mailbox("ses-x"), bus.root().join("ses-x.jsonl"));
        assert_eq!(bus.archive("ses-x"), bus.root().join("ses-x.archive.jsonl"));
        assert_eq!(
            bus.processing("ses-x"),
            bus.root().join("ses-x.jsonl.processing")
        );
    }

    #[test]
    fn unregister_removes_entry_but_keeps_mailbox() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        bus.register("ses-b", &[]).unwrap();
        bus.unregister("ses-b").unwrap();
        let names: Vec<String> = bus
            .list_agents()
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(
            names,
            vec!["ses-a".to_string()],
            "unregistered agent disappears"
        );
        // Mailbox survives: late messages are still deliverable (offline) and
        // the history is not lost.
        let outcome = bus
            .deliver("ses-b", &env_from("ses-a", "ses-b", "迟到的信"))
            .unwrap();
        assert!(outcome.delivered);
        assert_eq!(outcome.status, AgentStatus::Offline);
        assert_eq!(bus.read_and_archive("ses-b").unwrap().len(), 1);
        // Re-registering restores online status with history intact.
        bus.register("ses-b", &[]).unwrap();
        assert!(bus.list_agents().unwrap().iter().any(|a| a.name == "ses-b"));
    }

    #[test]
    fn unregister_unknown_name_is_noop() {
        let (_dir, bus) = test_bus();
        bus.unregister("ses-ghost").unwrap();
    }

    #[test]
    fn register_with_title_sets_and_preserves_title() {
        let (_dir, bus) = test_bus();
        bus.register_with_title("ses-a", &[], Some("修复登录 bug"))
            .unwrap();
        let title = bus
            .list_agents()
            .unwrap()
            .into_iter()
            .find(|a| a.name == "ses-a")
            .unwrap()
            .title;
        assert_eq!(title.as_deref(), Some("修复登录 bug"));
        // A title-less heartbeat keeps the stored title…
        bus.register("ses-a", &[]).unwrap();
        let title = bus.list_agents().unwrap()[0].title.clone();
        assert_eq!(title.as_deref(), Some("修复登录 bug"));
        // …and a fresh title replaces it.
        bus.register_with_title("ses-a", &[], Some("新标题"))
            .unwrap();
        let title = bus.list_agents().unwrap()[0].title.clone();
        assert_eq!(title.as_deref(), Some("新标题"));
        // New registrations without a title stay untitled.
        bus.register("ses-b", &[]).unwrap();
        assert!(bus.list_agents().unwrap()[1].title.is_none());
    }

    #[tokio::test]
    async fn notifier_fires_on_deliver() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        bus.register("ses-b", &[]).unwrap();
        let mut rx = bus.subscribe();
        assert!(!rx.has_changed().unwrap_or(false), "no deliveries yet");
        bus.deliver("ses-b", &env_from("ses-a", "ses-b", "hi"))
            .unwrap();
        assert!(
            rx.has_changed().unwrap_or(false),
            "deliver must bump the in-process notifier"
        );
        let _ = rx.borrow_and_update();
        bus.deliver("ses-b", &env_from("ses-a", "ses-b", "hi2"))
            .unwrap();
        assert!(rx.has_changed().unwrap_or(false), "every deliver notifies");
    }

    #[test]
    fn send_receipts_acks_read_messages_and_skips_receipts() {
        let (_dir, bus) = test_bus();
        bus.register("ses-a", &[]).unwrap();
        bus.register("ses-b", &[]).unwrap();
        let m1 = env_from("ses-a", "ses-b", "第一封");
        let m2 = env_from("ses-a", "ses-b", "第二封");
        bus.deliver("ses-b", &m1).unwrap();
        bus.deliver("ses-b", &m2).unwrap();

        let read = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(read.len(), 2);
        let receipts = bus.send_receipts("ses-b", &read);
        assert_eq!(receipts.len(), 2, "one receipt per read message");
        assert_eq!(receipts[0].to, "ses-a");

        // The sender sees two receipts, both referencing the originals and
        // typed `receipt`, carrying the recipient's reply address.
        let acks = bus.read_and_archive("ses-a").unwrap();
        assert_eq!(acks.len(), 2);
        assert!(acks.iter().all(|e| e.r#type == MessageType::Receipt));
        assert!(
            acks.iter()
                .any(|e| e.in_reply_to.as_deref() == Some(m1.id.as_str()))
        );
        assert!(
            acks.iter()
                .any(|e| e.in_reply_to.as_deref() == Some(m2.id.as_str()))
        );
        assert!(
            acks.iter()
                .all(|e| e.from == "ses-b" && e.reply_address.as_deref() == Some("ses-b"))
        );

        // Receipts are never acked: sending receipts for the receipts
        // produces nothing.
        let acks_of_acks = bus.send_receipts("ses-a", &acks);
        assert!(acks_of_acks.is_empty(), "no receipt loops");

        // Self-messages get no ack either.
        let self_msg = env_from("ses-b", "ses-b", "给自己");
        bus.deliver("ses-b", &self_msg).unwrap();
        let read = bus.read_and_archive("ses-b").unwrap();
        assert_eq!(read.len(), 1);
        assert!(bus.send_receipts("ses-b", &read).is_empty());
    }

    #[test]
    fn history_returns_newest_first_across_archive_and_mailbox() {
        let (_dir, bus) = test_bus();
        bus.register("ses-b", &[]).unwrap();
        let old = env_from("ses-a", "ses-b", "已读的旧消息");
        write_mailbox(&bus, "ses-b", &[old.clone()]);
        std::fs::rename(bus.mailbox("ses-b"), bus.processing("ses-b")).unwrap();
        bus.read_and_archive("ses-b").unwrap();
        let fresh = env_from("ses-a", "ses-b", "未读的新消息");
        write_mailbox(&bus, "ses-b", &[fresh.clone()]);

        let history = bus.history("ses-b", 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id, fresh.id, "mailbox (newer) comes first");
        assert_eq!(history[1].id, old.id, "archive (older) comes last");
        // The read-only view must not consume the mailbox.
        assert_eq!(bus.read_and_archive("ses-b").unwrap().len(), 1);

        let capped = bus.history("ses-b", 1).unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].id, fresh.id);
    }

    #[test]
    fn thread_id_roundtrips_through_the_wire_format() {
        let (_dir, _bus) = test_bus();
        let mut env = env_from("ses-a", "ses-b", "线程消息");
        env.thread_id = Some("thread-1".into());
        let line = serde_json::to_string(&env).unwrap();
        let decoded: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(decoded.thread_id.as_deref(), Some("thread-1"));
        // Old-format lines without the field still parse.
        let mut without = env.clone();
        without.thread_id = None;
        let line = serde_json::to_string(&without).unwrap();
        let decoded: Envelope = serde_json::from_str(&line).unwrap();
        assert!(decoded.thread_id.is_none());
    }
}
