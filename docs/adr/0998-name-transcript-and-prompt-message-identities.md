# ADR 0998：统一持久摘要与 prompt-history 消息身份字段

## 状态

已采纳并实施。

## 背景

两个消息身份边界仍使用泛名 `id`：Common `CanonicalMessage` 用可选身份关联 compaction 摘要气泡与 Memory episode，Agent 将其写入 append-only `CompactSummary` transcript event；Memory `SessionMessageText` 与 Agent `SessionPromptMessage` 在跨 crate 映射时也继续以 `id` 传递消息身份。

另外，ToolRun completion event 可经 outbox 重投。Agent 从稳定的 `tool_run_result_id` 派生同一个 `msg-*` transcript identity，避免重试重复插入消息；该确定性规则尚未作为 ID 规范例外说明。

## 决定

1. Common `CanonicalMessage` 的可选摘要身份改为 Rust 字段与 Serde 字段 `message_id`。新 durable event payload 只写 `message_id`；反序列化仅为历史 payload 接受 `id` alias。Agent validation 错误路径也使用 `compacted[].message_id`。
2. Memory `SessionMessageText` 和 Agent 私有 `SessionPromptMessage` 均使用 `message_id`，跨 crate 转换显式映射，不合并两个 owner 类型。
3. ToolRun completion 的 transcript `message_id` 继续由稳定 `tool_run_result_id` 确定性映射为 `msg-*`。这是一条 delivery-idempotency projection rule，不是普通的新实体 ID 生成；禁止改成每次投递随机生成。
4. 不改变数据库 schema、episode/message 行身份、provider tool-call ID 或 completion delivery 语义。

## 影响与验证

Common、Agent、Memory 与 LLM 使用的共享 Rust 类型字段统一为 `message_id`。既有 CompactSummary events 可继续 replay，新的 event JSON 只写明确字段。Prompt-history query DTO 保持 Memory owner，Agent prompt input 继续由 Agent owner 持有。

验证通过：`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check`、ADR 索引检查和 `git diff --check`。

## 回滚

Rust consumer 与 producer 字段可作为一个版本恢复为 `id`；历史 replay alias 随后可移除。数据库无需迁移。ToolRun completion identity 的确定性映射不能随字段改名而改变，否则可能重新投递已完成的 completion 时产生重复 transcript 行。
