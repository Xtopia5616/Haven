# ADR 0290：会话状态持久化通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：Agent `SessionSupervisor` / `SessionActor` 的会话状态持久化
- 关联：[ADR 0157](0157-session-supervisor-actor-run-engine.md)、[ADR 0172](0172-session-action-lifecycle-state-contract.md)、[ADR 0214](0214-react-run-inside-session-actor.md)、[ADR 0260](0260-session-store-session-creation-port.md)

## 背景

`SessionSupervisor::persist_status` 直接通过 `Arc<Database>` 调度状态写入；actor 的普通状态转换、dispatcher claim-run 和 executor miss 时结束会话都共用这条路径。`SessionSupervisor` 已经持有 `SessionStore`，但该状态写入仍越过其 typed persistence boundary。

actor 还通过同一个 `Arc<Database>` 执行交互事件追加等其他存储操作，因此本切片只迁移状态持久化，不移除 actor 的 raw Database 依赖。

## 决策

1. 为 `SessionStore` 增加异步 `update_session_status` 端口。它只将现有 `Database::update_session_status` 调度到 blocking pool，不引入 Agent 状态类型以外的新 facade 或 trait，也不改变底层 SQL。
2. `SessionSupervisor::persist_status` 改为调用该端口，并保留三次尝试、失败间 10ms 延迟和最终错误传播。普通 `transition`、`claim_run` 以及 actor miss 时的 `Completed` 写入都经过同一 helper/store 路径。
3. actor 仍只在持久化成功后更新 `SessionState`、`watch` 状态、运行状态和更新时间；失败时保留原状态。相同状态、非法转换和 `persist = false` 的行为不变。
4. actor 的交互事件及其他仍需原始数据库操作的路径继续使用 `Arc<Database>`。不扩展到 session 删除、清空或其他 raw DB 路径。

保留 Agent 直接调度 Database 会重复 SQLite blocking-pool 边界；将重试策略下沉为新的跨域通用机制则会扩大 Store 契约。本次只增加与现有业务边界匹配的 typed 方法，并保留 Agent 已有重试政策。

## 影响与验证

- 无 schema、IPC 或用户数据变化，也无数据库重置要求。
- Memory 测试确认状态写入经 `SessionStore` 持久化；Agent 测试覆盖成功转换、actor miss 的 Completed 写入，以及数据库失败时的三次尝试、错误传播和内存状态不变。
- 通过：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 和 `cargo test --workspace --locked --quiet`。Agent 497 项与 Memory 302 项测试通过；workspace 测试全部通过（其中 733 项通过、2 项忽略的 crate 测试套件也已通过）。

## 回滚

恢复 `persist_status` 与 actor 状态写入中的直接 `Database::run_blocking` 调用，并删除 `SessionStore::update_session_status`、对应测试、本 ADR、索引与路线图条目。无需迁移或重置数据库。
