# ADR 0298：ReAct event boundary cursor 通过 SessionStore 读取

- 状态：Accepted
- 日期：2026-09-25
- 范围：`event_boundary.rs` 中 pause/continue/error 共用的只读 event-boundary cursor 检查
- 关联：[ADR 0159](0159-session-event-store.md)、[ADR 0207](0207-session-store-replay-boundaries-and-durable-ui-sequences.md)、[ADR 0296](0296-react-transcript-event-store-ports.md)、[ADR 0297](0297-rollback-session-store-ports.md)

## 背景

ReAct 的 boundary 检查已不再写 snapshot；成功条件是 `SessionStore::load_replay_state` 能读取 replay cursor。该检查仍由 Agent 直接调用 `Database::run_blocking` 或 `run_blocking_cancellable`，并在闭包中同步调用 SessionStore，重复暴露 blocking-pool 调度。

boundary 的 pause/continue/error 决策、取消来源、指标和失败降级属于 Agent。Memory 只应提供 cursor 读取与数据库调度，不接收 error-partials 参数、`ReActState` 或 boundary 策略。

## 决定

1. `SessionStore` 增加异步端口 `event_boundary_cursor(session_id, cancel)`，返回 `SessionCursor`。端口内部复用同步 `load_replay_state`：存在 replay state 时返回其中 cursor，无 event log 时返回 `SessionCursor::default()`。
2. `cancel` 为可选 `CancellationToken`。有 token 时使用 `Database::run_blocking_cancellable`，无 token 时使用 `Database::run_blocking`，与现有调用选择完全一致；不改变底层 replay 查询。
3. Agent 的 `read_event_boundary_with_error_partials` 只调用该端口。boundary timer、失败计数器、warning 文本、错误转 `false` 与成功转 `true` 保持原样；成功仍表示 cursor 读取完整性检查通过。该检查不写投影、不追加事件。
4. `ReActEngine.db` 继续保留，因为 `persist_compaction_summary` 仍通过 raw `Database::run_blocking` 写入 compaction summary episode，并在 summary 达到既有长度阈值时写 pending extraction marker。compaction 路径不属于本切片。

将 `ReActState` 或 Agent 的 partial/error 策略放进 Memory 会倒置职责；在 Agent 再包一层 blocking 调度则会保留重复的存储细节。窄 cursor port 仅搬迁已有调度边界。

## 影响与验证

- 无 schema、IPC、用户数据契约或依赖变化，无需数据库重置。
- Memory 测试覆盖有/无 event log 的 cursor、可取消调度路径及取消结果。
- Agent 测试覆盖 boundary 读取成功/失败、失败指标、snapshot phase 指标，以及成功时不修改事件流；不测试日志。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

将 `event_boundary.rs` 恢复为原 `Database::run_blocking` / `run_blocking_cancellable` 包装，删除新增 SessionStore 端口及其测试、本 ADR、索引和路线图记录。无数据库迁移或重置步骤。
