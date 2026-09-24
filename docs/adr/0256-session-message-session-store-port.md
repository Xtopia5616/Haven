# ADR 0256：Ingress 与 recovery 消息通过 SessionStore 持久化

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 的 ingress/recovery 消息写入与 `haven-memory::SessionStore`
- 关联：[ADR 0196](0196-session-actor-event-sourced-state.md)、[ADR 0251](0251-partial-stream-session-store-port.md)、[ADR 0255](0255-transcript-batch-session-store-port.md)

## 背景

Agent 的 `persist_session_message_inner` 直接从 `SessionSupervisor` 取
`Database`，在 Agent 层执行 blocking SQLite 调度、按 `message_id` 检查幂等并写入消息。
这让 ingress/recovery 消息路径绕过已共享的 `SessionStore`，并在 Agent 中重复表达存储职责。

## 决定

1. `SessionStore::persist_session_message` 提供有语义的消息写入端口，拥有 blocking
   SQLite 调度并支持可选 `CancellationToken`；不开放通用 blocking 闭包接口。
2. 保持既有 `message_id` 幂等条件和冲突错误：同一 ID 的匹配内容返回 existing，冲突返回
   原错误；没有 ID 时调用 `add_message_full`。所有消息字段仍传入原持久化实现。
3. Agent ingress/recovery 共用该端口。常规写入仍先 discard partial；恢复保留 partial 的
   路径仍由调用者决定何时 discard。
4. 移除仓库内唯一调用后删除 `SessionSupervisor::db()` getter；`fail_pending_action_steps`
   等其他 raw Database 路径不在本决定范围内。
5. ReAct assistant/thought/ask/reasoning transcript 仍必须通过
   `apply_transcript` → `project_chat_message`，本端口不接管这些写入。

## 影响与验证

数据库 schema 与消息格式不变。SessionStore 测试覆盖无 ID 插入及完整字段、同 ID 幂等命中和
幂等冲突；Agent 仍使用同一持久化入口，X12 写入约束不变。验证包括 `cargo fmt` 与
`haven-memory`、`haven-agent` 相关测试。

## 回滚

回退本提交并恢复 Agent 对 `Database` 的 blocking 调度即可；无需数据库重置。
