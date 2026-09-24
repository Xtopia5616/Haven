# ADR 0296：ReAct transcript 事件读写通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：`ReActEngine` 的 durable replay state 读取、transcript event seed 与单条 transcript event 追加
- 关联：[ADR 0159](0159-session-event-store.md)、[ADR 0196](0196-session-actor-event-sourced-state.md)、[ADR 0238](0238-session-recovery-read-ports.md)、[ADR 0255](0255-transcript-batch-session-store-port.md)、[ADR 0294](0294-session-interaction-events-through-session-store.md)、[ADR 0295](0295-session-action-step-writes-through-session-store.md)

## 背景

`ReActEngine` 的 `load_durable_event_state`、`seed_transcript_events` 和 `append_transcript_record` 都通过 raw `Database::run_blocking` 调度同步 `SessionStore` 操作。Agent 已持有 `SessionStore`，继续直接编排 blocking pool 会重复暴露存储调度边界。

事件记录的类型解析、错误文本、分支点到 ReAct 表示的映射与状态投影属于 Agent。Memory 只负责把既有同步读写放到 blocking pool，并保留现有事务、校验、提交后 broadcast 和兼容语义。

## 决策

1. `SessionStore` 增加 `load_replay_state_async`、`seed_if_empty_async` 与 `append_transcript_async` 三个具体异步端口。端口内部复用现有同步实现；不新增 trait 或依赖。
2. 单条 transcript 追加端口先检查 session row。缺失时维持现有兼容行为，返回 sequence `0` 且不写事件；session 查询或追加失败继续传播现有错误。成功时仍由同步 `append_transcript` 分配 sequence，并只在事务提交后广播。
3. `ReActEngine::load_durable_event_state` 从端口取得 raw replay state 后，继续在 Agent 解析 `TranscriptRecord`、保持现有解析错误文本并映射 branch points。seed 的 typed record 序列化与输入次序仍由 Agent 负责；单条追加仍在 Agent 序列化 record 并维护 event-append metrics。
4. 本切片只改上述三条 `event_boundary.rs` 路径。ReActState projection、transcript batch writer、compaction summary、branch-point 写入、rollback、recovery persistence 与 resume/rollback 编排不变；`ReActEngine.db` 仍被其他路径使用，暂时保留。

直接在 Agent 调度 Database 会继续让上层持有 blocking 细节；将 `TranscriptRecord` 或 ReAct replay policy 下沉到 Memory 会反转职责。具体端口只复用既有 SessionStore 同步实现。

## 影响与验证

- 无 schema、IPC、用户数据契约或依赖变化，无需数据库重置。
- Memory 测试覆盖 replay/seed/append 成功与顺序、重复 seed 不重复 broadcast、missing-session 返回 `0`，以及追加校验失败不落库、不 broadcast。
- Agent 测试覆盖 seed、单条追加、replay 解析和 branch point 投影；同时验证 synthetic missing-session 兼容、过大 transcript 失败不产生 durable/UI/消息副作用，以及 replay 解析错误文本不变。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

将三条 Agent 路径恢复为当前 SessionStore 同步实现外包一层 `Database::run_blocking` 的调用，删除新增异步端口、相关测试、本 ADR、索引与路线图记录。无数据库迁移或重置步骤。
