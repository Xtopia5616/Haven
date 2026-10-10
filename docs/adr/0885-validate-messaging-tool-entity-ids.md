# ADR 0885：校验 Agent 工具中的 session、message 与 claim ID

## 状态

Accepted — 2026-10-10

## 背景

Agent/Messaging 工具把当前会话、peer、parent、reply message 和 inbox claim 都作为字符串处理。`session_of` 与目标路径原先只调用宽松的 agent-name 检查，显式 `in_reply_to` 和 `claim_token` 则主要依赖 schema；原生 `AgentTool::run` 路径可以绕过 schema。无效身份可能在 mailbox 注册后才失败，或被误认为不存在的 peer。

## 决定

- 当前 `_session_id`、peer/target/parent session 使用 `ses-*`；回复与 selective ack 的 message 引用使用 `msg-*`；inbox claim token 使用 `claim-*`。所有格式判断委托 Tools 对 Common `is_canonical_id` 的包装器。
- 在 mailbox 注册、收件箱领取或对 peer 执行操作前校验模型提供的 ID；schema 同时描述 canonical pattern。`message_ids`、显式 `in_reply_to`、`claim_token`、`target`、`parent` 与 `to` 共用对应实体空间。
- `send` 的 `to="*"` 是广播路由标记，不是 session ID；其他 send/request/reply 收件人必须是规范 session ID。
- 不继续接受旧的任意 agent-name 字符串作为 session identity，也不保留兼容 schema。Envelope、持久消息与其它工具输出中的字段 owner 仍按各自跨层审查推进，本 ADR 仅收紧 Agent 工具参数入口。

## 影响与兼容性

规范 ID 的有效调用及 peer authorization 语义不变。非法或旧格式 session、message、claim ID 会在注册 mailbox 或执行副作用前失败。没有持久化或 IPC schema 变更；无数据库重置需求。

## 验证

通过：`cargo fmt --all -- --check` 与 Agent/Messaging 工具测试（31 passed）。完整 `haven-tools` 测试、Clippy 与 workspace 门禁在本轮最终验证中执行。

## 回滚

若规范生成的 session/message/claim ID 被拒绝，应修复对应实体的生成或来源；不得重新接受宽松 agent name 作为 ID。
