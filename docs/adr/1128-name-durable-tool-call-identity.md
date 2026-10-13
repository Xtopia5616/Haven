# ADR 1128：统一持久工具调用身份字段

## 背景

Common `CanonicalToolCall` 表示 provider 发起的工具调用。其 Rust 字段和 durable `TranscriptRecord::ToolCall` 中嵌套的 JSON key 使用泛名 `id`，与 `CanonicalMessage.tool_call_id`、Agent 计划和执行 DTO 的 `tool_call_id` 命名不一致。provider ID 是不透明外部身份，不能因字段改名而转换其值或改变各协议 wire key。

## 决定

1. `CanonicalToolCall.id` 改为 `tool_call_id`。新 durable ToolCall payload 只写 `tool_calls[].tool_call_id`；serde reader 仅保留 `id` 作为历史 event replay alias。
2. Agent、Common 和 LLM 中的构造与访问统一使用 `tool_call_id`。provider adapters 显式把各自的入站 ID 映射进该字段，并在出站时映射回协议规定的字段（例如 Chat/Anthropic/Gemini `id`、Responses `call_id`），原值逐字保留。
3. replay alias 只服务于 durable transcript 读取；provider raw payload 由各 adapter 映射，不依赖 Common serde alias。无 schema、event version 或数据库列变化。

## 替代方案

- 只给序列化字段加 `rename`，保留 Rust `id`：拒绝。Rust API 仍使用无领域含义的短名，调用者也无法从字段识别它与消息 `tool_call_id` 的关系。
- 移除旧 payload alias 并要求数据库重置：拒绝。已有 append-only ToolCall event 可由反序列化 alias 原样读取；schema 和事件结构未变，不应为字段规范化清空会话数据。
- 改写 provider ID 或让 provider payload 直接反序列化 Common 类型：拒绝。provider ID 属于外部协议，adapter 必须拥有协议映射并保持原值。

## 影响与验证

Common、Agent、LLM 的 Rust API 与新 durable payload 统一使用 `tool_call_id`。历史 payload 的 `tool_calls[].id` 可继续 replay；provider 请求/响应 wire shape 与 tool-call 关联行为不变。不需要数据迁移或重置。

测试覆盖 Common 新序列化与旧别名读取，以及 Agent durable ToolCall payload 的新写入、历史 `id` replay 和 identity validation。`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check` 与 `git diff --check` 均通过。

## 回滚

可将 Rust producer/consumer 字段整体恢复为 `id` 并重新输出旧 payload key；已有 replay alias 可随即移除。数据库 schema 无需迁移。provider wire mapping 不随内部字段回滚而改变。
