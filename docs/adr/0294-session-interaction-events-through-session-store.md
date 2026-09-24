# ADR 0294：SessionActor 交互事件通过 SessionStore 异步端口

- 状态：Accepted
- 日期：2026-09-24
- 范围：SessionSupervisor/SessionActor 的 interaction domain event 读写
- 关联：[ADR 0157](0157-session-supervisor-actor-run-engine.md)、[ADR 0159](0159-session-event-store.md)、[ADR 0196](0196-session-actor-event-sourced-state.md)、[ADR 0214](0214-react-run-inside-session-actor.md)、[ADR 0290](0290-session-status-persistence-through-session-store.md)

## 背景

`SessionSupervisor::install_actor` 通过 `Database::run_blocking` 包裹同步 `read_active_domain_events`，再调用 Agent 的 interaction replay reducer。`SessionActor::spawn` 也持有 `Arc<Database>`，交互请求、resolved 结果和清理事件经 raw Database 调度后调用 `SessionStore::append`。两端都已有 SessionStore，但这条路径仍重复传播 SQLite blocking-pool 调度边界。

交互 payload 解析、状态选择和 actor 内存状态仍属于 Agent。Memory 只应调度现有 domain event 读写，不负责解析 `InteractionRequest` 或决定如何恢复交互。

## 决策

1. `SessionStore` 增加 `read_active_domain_events_async` 与 `append_domain_event` 异步窄端口，分别在 blocking pool 调用既有同步读取与 `append`。追加端口固定 `run_id=None`、`step_number=None`；原有 payload、event type、错误结果及 `append` 的提交后 live broadcast 语义不变。
2. `actor::load_interactions` 改为异步调用 SessionStore 端口。`SessionSupervisor::install_actor` 直接 await replay；失败时继续 warning 并用空 interactions 安装 actor。interaction payload 解析、清理规则和恢复 reducer 留在 Agent。
3. `append_interaction_event` 只接收 SessionStore。SessionActor 不再接收 raw Database 参数；requested、resolved、cleared 事件均须 durable append 成功后才更新内存状态。resolved payload 在持久化前从请求副本构建，失败时 actor 状态保持 pending。
4. `SessionActor::spawn` 不再接收 raw Database。`SessionSupervisor` 保留现有生产 `Database` 字段，因为 `session/tool_runner.rs` 的 action-step 持久化仍使用它；该字段不再用于 interaction event replay，也不再传入 actor。AgentLayer、ReActEngine 及其他尚未迁移的模块仍保留自己的 raw Database 依赖，不属于本次切片。

由 Agent 直接调度 Database 会继续重复存储 blocking 边界；将 interaction payload 或 replay policy 下沉至 Memory 则会反转域职责。本端口仅复用原 SQL/event 实现，不增加通用 storage abstraction。

## 影响与验证

- 无 schema、IPC、事件 payload 或用户数据契约变化，无需数据库重置。
- Agent 测试覆盖交互事件持久化及新 supervisor 从 durable event replay actor interaction。Memory 测试覆盖异步端口的读写、默认 run/step 值和 live broadcast。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo clippy --locked -p haven-memory -p haven-agent -- -D warnings`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

恢复 install_actor 的 Database blocking 调度与 SessionActor 的 Database spawn 参数，并删除新增 SessionStore 异步端口、本 ADR、索引与路线图记录。Supervisor 的既有 Database 字段继续为 tool_runner action-step 路径保留。无需迁移或重置数据库。
