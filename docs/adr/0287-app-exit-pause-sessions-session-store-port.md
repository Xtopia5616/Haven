# ADR 0287：退出时通过 SessionStore 同步暂停运行会话

- 状态：Accepted
- 日期：2026-09-24
- 范围：App Tauri `RunEvent::Exit` 与 Memory `SessionStore`
- 关联：[ADR 0269](0269-memory-worker-shutdown-boundary.md)、[ADR 0283](0283-app-session-record-lookups-through-session-store.md)、[ADR 0286](0286-app-notification-session-display-title-session-store-port.md)

## 背景

Tauri 的 `RunEvent::Exit` 回调是同步回调。当前 App 在该回调中直接调用
`state.db.pause_running_sessions()`，再执行 `ApplicationRuntime::teardown_blocking()`。
`ApplicationRuntime` 已注入 `SessionStore`，但退出适配层仍绕过这个 typed persistence
boundary 访问 Database。

退出时暂停运行会话是崩溃恢复契约的一部分：正常退出把仍为 `running` 的会话改为
`paused`，启动时的 `finalize_orphaned_running_sessions` 才只把崩溃遗留的 `running` 会话改为
`error`。此调用不能为迁移到 SessionStore 而变成异步。

## 决策

1. 为 `SessionStore` 增加同步窄方法 `pause_running_sessions()`，直接委托给既有
   `Database::pause_running_sessions()`。底层 SQL、事务行为、状态转换、返回更新行数和错误
   传播均沿用现有实现。
2. `RunEvent::Exit` 使用 `ApplicationRuntime` 注入的 `SessionStore` 调用该方法。保持同步
   回调、`n > 0` 时的 info 日志、计数为 0 时静默、错误日志文本与脱敏方式不变；暂停操作
   仍先于 `teardown_blocking()`。
3. 不新增通用 trait，不改变其他 shutdown 路径、数据库 schema、IPC 或崩溃恢复语义。
4. 删除 `ApplicationRuntime` 已无生产调用者的 raw `Database` 字段；`RuntimeServices` 仍在
   装配时将 Database 交给既有 `MemoryFactStore`。这也让 App runtime 不再暴露未使用的原始
   数据库句柄。

保留对 Database 的直接调用虽可工作，但会继续让 App 生命周期适配层越过已有 store 边界。
用异步端口或在退出回调中启动异步工作则改变同步退出时序，因此不采用。

## 影响与验证

- Memory SessionStore 测试覆盖多条 running 会话的更新计数、paused/pending/completed 状态
  保持，以及重复调用返回 0。
- App bootstrap helper 测试覆盖通过注入的 SessionStore 将 running 会话暂停。
- `ApplicationRuntime` 不再保留未使用的 raw Database 字段；AppState 中的定时任务停机测试
  从其临时数据库文件重新打开并检查记录。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory session_store_`
  （19 项）、`cargo test --locked -p haven-app-binary`（148 项）、
  `cargo clippy --locked -p haven-memory -p haven-app-binary -- -D warnings`、
  `cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、
  `cargo test --workspace --locked`。
- 无持久化格式或数据迁移；退出状态转换保持原样。

## 回滚

可恢复 `RunEvent::Exit` 对 `state.db.pause_running_sessions()` 的调用，并删除
`SessionStore::pause_running_sessions()`、对应测试、本 ADR 及索引/路线图条目。无需重置或
迁移数据。
