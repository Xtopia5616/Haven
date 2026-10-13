# ADR 1116：为已删除会话保留 Messaging 谱系墓碑

## 状态

已接受（2026-10-13）

## 背景

显式、retention 与批量 session 删除只清理 Agent/SQLite 状态，不清理 Messaging registry 或 mailbox。`unregister` 是正常关闭语义：它移除 liveness entry，但保留 mailbox/archive，并允许迟到消息与后续重注册。JSONL claim 会在 ack 前把 envelope 写入 archive，因此 archive 不是“已读”或“已确认”的标记。仅从工具层屏蔽 history 也会留下直接 service 调用、自动 inbox polling、旧 `MessageClaim` ack 与旧 heartbeat 入口。

## 决定

- 增加 `deleted_sessions.json`，以 `session_id -> parent_session_id?` 保留最小持久谱系。它不保留 title、role、capabilities、message body、liveness 或删除时间。写入和 registry/mailbox 操作共用 JSONL transport 的 `.lock`，并使用可恢复的原子替换。
- `SessionSupervisor` 的单会话、retention 与批量删除路径在 SQLite 删除前先 tombstone Messaging。tombstone 首次落盘后即为永久删除意图；后续清理失败保持 fail-closed，可通过重复删除幂等完成。若随后 SQLite 删除失败，会话行仍在，但该 ID 的 Messaging 操作继续被封锁，调用者应重试删除。
- `delete_all_sessions` 在数据库 session IDs 之外，还扫描 registry、既有墓碑及 canonical mailbox/processing/archive 文件名，以 tombstone 旧版本显式删除遗留的 unregistered mailbox。
- tombstone 后，注册/heartbeat、向其投递、以其为发送方投递、claim/try-claim、ack、history、last-received、find-message 与 selective reply lookup 均失败。stale claim 的完成不发送 receipt，也不重建 mailbox。广播 agent/child 列表隐藏墓碑；descendant BFS 可以穿过墓碑节点，但不会将墓碑本身作为可操作后代返回。
- Transport 没有独立的人类已读标记；删除 session 时清除其 mailbox、`.processing`、bounded archive 及对应临时文件中的全部消息正文，包括此前已 ack 的 archive 内容。先持久化 tombstone，再清理正文文件；中断时已删除身份保持 fail-closed，重复删除继续完成清理。已投影到其他存活 session canonical transcript 的内容不因收件 session 删除而撤回。
- 保持正常 end/unregister/mark-offline 语义不变。删除 child 后保留的 `parent_session_id` 继续穿透 descendants；child spawn cap 仍按累计注册身份计数，包含 tombstone，以兼容 ADR 1101 的不可释放名额规则。
- 术语上，`MessageClaim` 是进程内未确认批次句柄，不授予独占领取权；相同稳定 `message_ids` 可被多个 consumer 观察，直至其中一个 ack。archive 是 delivery audit tail，不代表 ack 状态。
- 不为 mailbox 或 `.processing` 增加累计容量，也不改变 stable message ID、claim/ack 或重投语义；容量和旧 backlog 有界排空作为后续独立切片。

## 替代方案

- 复用 `unregister` 会错误删除 parent 关系，并继续保留可接收消息的 mailbox；同 ID heartbeat 还可复活 registry。
- 仅删除 mailbox 会留下 `.processing` 与 archive 中的消息正文；旧 ack 还可能重建空 mailbox。
- 清空 registry 或隐藏 parent link 会断开 child lineage 与 descendants authorization。
- 将 MessageClaim 描述为 durable/exclusive lease 与实际稳定 ID、无 owner/token/expiry 的 ack 行为不一致。

## 影响

不修改 SQLite schema、IPC shape、Envelope 字段或 `unregister` 行为，无数据库重置要求。新增 Messaging 持久文件；单会话删除只 tombstone 指定 ID，批量清理会回收已留在 JSONL 文件中的旧 unregistered identity。删除事务跨 SQLite 与 JSONL，采用先写 Messaging 删除意图的 fail-closed 顺序，明确保留上述 SQLite 失败时需要重试的行为。

## 验证

- Messaging transport 测试 tombstone 不可复活、所有读/写/claim/ack 入口拒绝、包括已 ack archive 与临时文件在内的全部 session 消息正文清理、stale claim 不重建邮箱/发 receipt、descendant 穿透与 child cap 累计，以及批量清理发现旧 unregistered mailbox。
- Agent 测试单删、批删、retention 复用的 deletion 路径写入 tombstone。
- 运行 `cargo test --locked -p haven-messaging`、`cargo test --locked -p haven-agent`、`cargo test --workspace --locked`、workspace check/clippy、IPC contract/event 与 ADR index 门禁。

## 回滚

不得只移除 API tombstone 检查或恢复可重注册行为，否则持久化已删除 session 会重新接收消息。若回滚删除行为，必须定义 tombstone 文件兼容与安全删除策略，并证明跨进程旧 sender/claim 不会重开邮箱。
