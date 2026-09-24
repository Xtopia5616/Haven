# ADR 0288：Agent 标题生成上下文通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：Agent 后台会话标题生成的持久化读取与写入
- 关联：[ADR 0277](0277-context-source-session-title-port.md)、[ADR 0281](0281-session-title-write-session-store-port.md)、[ADR 0284](0284-end-session-display-title-session-store-port.md)

## 背景

`AgentLayer` 的后台标题生成路径仍直接持有 raw `Database`：它在一个 blocking closure
中读取 session、检查已有 title，并读取用于生成标题的消息；生成成功后再由另一个
blocking closure 写入标题。Agent 因此重复承担了 SQLite 调度和标题上下文查询细节，尽管
`SessionSupervisor` 已提供 `SessionStore`，且标题写入端口已存在。

## 决策

1. 在 `SessionStore` 增加 typed `title_generation_context` 端口和
   `SessionTitleGenerationContext` DTO。该端口在一次 `Database::run_blocking` 中读取 session、
   检查 title，并通过既有 `get_session_messages_limit` 读取最多 10 条可用于对话的消息；
   随后仅保留 user 消息，保持原有时间顺序。session 缺失或 title 已存在时返回 `None`；
   untitled session 返回 DTO，即使其中没有 user 消息。
2. `spawn_title_generation` 从 `SessionSupervisor::session_store()` 取得 store。Agent 只在
   DTO 含有 user 消息时调用生成器；读取失败仍记录原 warning 并停止本次生成。
3. 生成成功后复用 `SessionStore::update_session_title`。持久化成功后仍先更新 executor，再
   发布 `TitleUpdated` 并记录成功日志；写入失败继续记录原 warning，不更新 executor 或发布事件。
4. 保留单 session 的 in-flight 去重、LLM 请求、缺失 session/已有标题的短路语义、错误降级、
   SQLite blocking 调度和事件顺序。AgentLayer 的 peer inspect、peer title、rollback/delete
   等其他 raw Database 路径不属于本 ADR；不改变 schema、IPC 或 provider 契约。

在 Agent 内分别执行 session/title/messages 查询仍会重复 Store 的 SQLite 边界；新增 Database
facade 或拆成多个独立 blocking 查询也会增加重复接口或改变原有单 closure 查询行为，因此不采用。

## 影响与验证

- 标题生成路径仅依赖 `SessionStore`，使用 typed user-message context；标题写入继续复用既有端口。
- Memory 测试覆盖缺失 session、已有 title、user-only 过滤以及最新 10 条的上限和顺序；Agent
  标题生成调用链复用该 DTO 和既有写端口，行为由相关 crate 测试验证。
- 验证通过：`cargo fmt --all -- --check`、Memory 测试（301 项）、Agent 测试（494 项）、
  两 crate 严格 Clippy，以及 `cargo check --workspace --locked`、
  `cargo clippy --workspace --locked -- -D warnings` 和 `cargo test --workspace --locked`。

## 回滚

恢复 Agent 标题路径中的 `Arc<Database>` 读取和标题更新调用，并删除
`SessionStore::title_generation_context`、DTO、测试、本 ADR、索引和路线图条目。无 schema 或数据
迁移要求。
