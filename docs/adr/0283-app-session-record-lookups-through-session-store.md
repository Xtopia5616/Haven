# ADR 0283：App 会话记录读取通过 SessionStore 异步端口

- 状态：Accepted
- 日期：2026-09-24
- 范围：App `get_session_for_resume` 与 `get_last_conversation` 的 session record lookup
- 关联：[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0279](0279-app-history-session-store-ports.md)、[ADR 0282](0282-app-session-resume-projection-session-store-port.md)

## 背景

ADR 0282 已将 resume response 的 messages、steps、usage 和 active domain events 读取
移到 `SessionStore`，但两个 Tauri command 仍直接通过 `AppState`/`Database` 读取 session
record：`get_session_for_resume` 调用按 ID 查询，`get_last_conversation` 调用
`list_sessions(1, 0)`。这些同步 SQLite 查询仍在 async command 路径上执行，也让命令适配层
保留了 raw Database 读取依赖。

Agent 生命周期调用仍需要现有同步 `SessionStore::session_record`。将该同名 Rust 方法改成
async 会改变调用契约并与同步用法冲突。

## 决策

1. 新增异步 `SessionStore::load_session_record(session_id)`，在
   `Database::run_blocking` 中复用 `get_session`。原同步 `session_record` 保留给现有
   Agent 调用；两者都将缺少记录表示为 `Ok(None)`。
2. 新增异步 `SessionStore::latest_session_record()`，复用
   `list_history(1, 0)` 并取首条记录。因此沿用既有 `created_at DESC`、limit/offset、
   first-page cache 行为，并在没有 session 时返回 `Ok(None)`。
3. 两个 App command 均通过新增端口读取 session record。`get_session_for_resume` 继续将
   缺失记录映射为精确错误 `Session not found: {id}`；`get_last_conversation` 继续在无记录时
   返回 `None`。后续 resume projection、interaction 映射与 IPC response 不变。
4. 本切片不改 `end_session` fallback、消息/附件恢复路径、数据库 schema 或 IPC 契约。

这些异步端口将 SQLite work 放入 Tokio blocking pool。调用方丢弃 future 不会中止已经开始的
blocking closure。

## 影响与验证

- App 命令不再为这两个 session record lookup 直接读取 raw `Database`。
- Memory 测试验证 async 按 ID 端口的存在/缺失语义及最新 session 选择、空结果和既有列表排序。
- App 测试验证精确 not-found 错误、最近会话返回、无会话时 `None` 与 resume response 现有
  IPC 字段。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory session_store_`
  （17 项）、`cargo test --locked -p haven-app-binary`（141 项）和
  `cargo clippy --locked -p haven-app-binary -- -D warnings`。

## 回滚

恢复两个 command 原有的同步 `Database` 查询，并删除新增 SessionStore 方法及对应测试。
不涉及持久化数据或数据重置。
