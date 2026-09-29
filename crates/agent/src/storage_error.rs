//! User-facing guidance for local SQLite storage failures.

/// Return a bounded recovery message for SQLite capacity and disk I/O errors.
/// Disk I/O failures remain distinct from `SQLITE_FULL` because their cause
/// can be unrelated to exhausted space.
pub fn sqlite_storage_failure_message(error: &anyhow::Error) -> Option<&'static str> {
    match haven_memory::SessionStore::sqlite_storage_write_failure(error)? {
        haven_memory::repositories::session_events::SqliteStorageWriteFailure::DiskFull => Some(
            "数据库所在磁盘空间不足，本次写入失败。先释放该磁盘空间；未提交的输入请重新发送，会话停止时可点“继续生成”重试。删除会话不保证缩小 SQLite 文件。",
        ),
        haven_memory::repositories::session_events::SqliteStorageWriteFailure::IoFailure => Some(
            "SQLite 本地数据库发生磁盘 I/O 错误，本次写入失败。请检查数据盘空间和磁盘可用性，恢复后重发未提交内容或点击“继续生成”；删除会话不保证缩小 SQLite 文件。",
        ),
    }
}
