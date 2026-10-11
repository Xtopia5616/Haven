# 0963：在 Memory 持久边界校验 transcript identity

## 状态

已接受并实现（2026-10-11）。

## 背景

Memory message 与 session-step repository 过去信任调用方传入的 session、message 和 step ID；持久行 mapper 也原样返回数据库 ID。测试夹具因此长期使用短 ID，掩盖了 owner 边界对 `{prefix}-{uuid32}` 的漏检。

`FactSourceRef.message_id` 由 Agent extraction 或 Memory fact command 提供，FactGraph 会把它持久化为 `provenance_item_id` 或 `provenance_record_id`。此前任意非空文本都能写入该实体引用。

消息和步骤投影存在有意共享身份：普通 transcript 与 pending input 用 `msg-*`；assistant thought / ask 的消息行与步骤行共用 `step-*`；supplement 的 thought 步骤与用户消息共用 `msg-*`。action session-step 使用 `step-*`。

## 决定

- Message repository 校验 `session_id` 为 canonical `ses-*`，transcript `id` 为 `msg-*` 或共享投影的 `step-*`；pending user input 与 pending marker 只接受 `msg-*`。
- Session-step action 写入只接受 `ses-*` session 与 `step-*` step；thought step 接受 `step-*` 或 supplement 共用的 `msg-*`。
- Message、pending input 与 session-step row mapper 校验从 SQLite 读取的身份；畸形持久身份使查询报错，不向上游返回伪实体。
- FactGraph 对非空 `FactSourceRef.message_id` 使用 Common canonical checker 要求 `msg-*`，并在节点、事实或强化更新前验证；空 ID 保留既有“仅片段、无实体引用”的语义。
- 所有校验均使用 `haven_common::types::is_canonical_id`。不增加 schema 约束或迁移。

## 替代方案

- 只校验 Tauri / Agent 上游输入：会让其他合法调用方绕过 Memory owner 的持久约束。
- 所有消息行都限制为 `msg-*`：会破坏 thought/ask 跨 messages 与 session_steps 的身份关联。
- 把 thought step 限制为 `step-*`：会破坏 supplement 消息与其执行锚点的既有共用 ID 契约。
- 在 Message DTO 改用新类型或更改字段结构：当前的 Common string ID wire contract 已稳定，且此问题可由持久 owner 校验解决。

## 影响

合法的规范 ID 与现有共享身份路径保持不变。畸形 ID 在写入前失败，畸形持久行在读出时失败；不会创建部分消息、步骤、事实节点或事实行。数据库 schema、IPC JSON 形状、event replay 和 rollback 双时钟均不变。测试夹具改为 Common 生成器创建真实格式的 ID。

## 验证

- Memory message 写入、pending 输入、message row 读取、thought/action step 共用身份、step row 读取与事实 provenance 均有回归覆盖。
- `cargo test --locked -p haven-memory`
- `cargo test --locked -p haven-tools builtin::memory::tests::test_source_snippet_skips_sensitive_text`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

若未来改变 transcript 与执行行的 ID 共用关系，应先调整事件投影、history rebuild 与前端身份关联，再更新这些 repository guards。不得以内容比对替代实体身份。
