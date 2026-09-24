# ADR 0300：Agent resume 媒体读取通过 SessionStore read model

- 状态：Accepted
- 日期：2026-09-25
- 范围：`AgentLayer::run_session_from_id` 的初始消息媒体与 session 附件读取
- 关联：[ADR 0233](0233-session-managed-asset-lease-port.md)、[ADR 0280](0280-fresh-run-conversation-window-session-store-port.md)、[ADR 0297](0297-rollback-session-store-ports.md)

## 背景

`resume.rs` 仍通过 `AgentLayer.db` 调度一条 `Database::get_session_messages` 查询，并在 Agent 聚合初始 user 消息的 id、attachments、`media_inputs`，以及当前 session 的全部 attachments。这是该文件唯一的生产 raw Database 读取。查询属于 SessionStore 的消息持久化边界；聚合输出只供 Agent 初始化输入及重新登记受管资产使用。

该读取不是恢复 transcript 的 authority。`session_events` 仍是 ReAct 恢复的唯一 durable authority；消息及媒体只提供既有物化输入。SessionStore 也不管理进程内媒体租约或注册。

## 决定

1. `haven-memory` 公开 `SessionResumeMedia` DTO，承载 `initial_message_id`、`initial_attachments`、`initial_media_inputs` 与 `all_attachments`；`SessionStore::session_resume_media` 在单个 `run_blocking` closure 中读取消息并生成该 read model。
2. 复用 `Database::get_session_messages` 的原顺序。仅首个 `role == "user"` 消息作为初始输入；没有该消息时 id 为 `None`、媒体字段为空 vec。空 attachments 或 `media_inputs` 保持为空 vec。`all_attachments` 按消息顺序 flatten，查询仍按 session 隔离。
3. `resume.rs` 调用 `self.executor.session_store().session_resume_media(session_id).await`，并继续将存储错误映射为 `failed to load session resume data: {error}`。Agent 继续负责 `register_managed_assets_for_session`、canonical initial input、事件加载及 fresh-run/resume 分界；这些操作的顺序不变。
4. Managed asset lease/register 所有权继续属于 Agent/SessionSupervisor（ADR 0233）。SessionStore 只读持久化消息投影；不解析或回放事件、不存放 ReAct 状态、不注册媒体资产，也不创建租约。
5. `AgentLayer` 不再保留只供旧 resume 查询使用的生产 `Arc<Database>` 字段；构造时仍将 Database 注入 MemoryService/MemoryStore 等既有 owner。测试构建保留该字段供既有断言使用。

该窄 read model 复用现有消息查询和 blocking-pool 边界；把事件 replay 或租约副作用带入 Memory 会混合独立的权威与所有权。此切片不改变 schema、IPC、Cargo 依赖或其他 MemoryWorker/raw Database 路径。

## 影响与验证

- Memory 测试覆盖消息顺序、首条非 user 后的首个 user、初始媒体为空与 session 隔离；Agent resume 集成回归覆盖初始 user 媒体的既有 canonical 初始化行为。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。
- 无数据格式变化，无需数据库重置。

## 回滚

恢复 `resume.rs` 中原有的消息读取与媒体聚合，并删除 `SessionResumeMedia`、`session_resume_media`、相应 Memory 测试、本 ADR、README 索引和路线图记录。无需迁移或重置数据库。
