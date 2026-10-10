# ADR 0886：在 Messaging 存储边界统一 session 与 message 身份契约

## 状态

Accepted — 2026-10-10

## 背景

ADR 0885 收紧了 Agent 工具参数，但 `MessagingService` 的 `SessionMailbox` 分支和公开的 `InboxBus` 仍有可绕过入口。此前只有部分路径校验 session 名称或 message id；直接调用 transport 可以把非规范身份写入 JSONL，读取已有 registry 与历史行时也没有复核其字段。

## 决定

- Messaging 以一个私有 `contract` 模块作为 session、message 和 envelope 的校验 owner；所有格式判断调用 Common 的 `is_canonical_id`。
- `MessagingService` 在调用 transport 或 `SessionMailbox` 前校验 session、message 和 request/reply 引用；runtime 返回的 spawned/control session id 也在返回给调用方前校验。
- `InboxBus` 的注册、路由、claim、ack 和查询入口校验各自身份字段；直接 delivery 同样先验证完整 envelope，之后才写 mailbox。
- registry 读取和写入都要求 map key、entry identity 与 parent session id 相符且规范。mailbox 与 archive 的 JSONL 读取只有在 envelope 满足同一契约并且 `to` 与所在 session mailbox 一致时才接受该行；无效行不进入 claim、history、reply lookup 或 archive 去重结果。
- 不迁移旧的 agent-name registry、session ID 或不合规 envelope。旧 registry 会明确报错；按下方发布文档清空 `inbox/` 后重新使用。Transcript 仍在 SQLite 中，重置 inbox 不删除会话正文。

## 影响与兼容性

规范 session/message ID 与有效 envelope 的行为保持不变。非规范输入在 transport、副作用和内存 mailbox 读取之前失败；旧格式 inbox 数据不再可读。JSONL envelope 的跨进程字段形状不变，session 的 canonical 格式与 `msg-*` message id 由现有项目约定定义。

## 验证

通过：`cargo test --locked -p haven-messaging --lib`（50 passed）。Workspace 测试及全量 Clippy 在本轮最终门禁执行。

## 回滚

若当前生产入口生成的 session/message ID 被拒绝，应修正对应来源或注册流程；不恢复宽松 agent-name 校验，也不静默接纳旧 registry 与 envelope。
