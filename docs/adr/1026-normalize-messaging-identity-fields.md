# 1026：统一 Messaging 持久身份字段

## 状态

已采纳并实施（2026-10-12）。

## 背景

Messaging envelope 与 Agent registry 使用 `id`、`from`、`to`、`name`、`parent` 等泛称字段；Messaging 工具参数、结果和 UI renderer 又重复消费这些字段。它们分别承载消息 ID 或 session ID，省略实体名导致跨进程 JSONL、Rust DTO、工具 schema 和 UI view 的概念不一致。

## 决定

- Envelope 持久字段统一为 `message_id`、`from_session_id`、`to_session_id`、`reply_to_session_id` 和 `in_reply_to_message_id`；reply target accessor 也使用 `reply_target_session_id`。
- Registry 和 AgentInfo 使用 `session_id`、`parent_session_id`；SendOutcome 使用 `to_session_id`。
- Agent 工具收件人和生命周期目标参数使用 `to_session_id`、`target_session_id`；工具输出及 UI 专用 renderer 同步消费 `session_id`、`parent_session_id` 与 envelope 的新字段。
- 不提供旧字段 alias。旧 JSONL envelope 和 `agents.json` 记录不会被新版本读取；升级时退出所有共享 inbox 的 Haven 进程并重置整个 `inbox/`，SQLite transcript 无需重置。

## 影响

此变更统一持久文件与工具边界中的实体身份含义，不改变消息路由、确认、回复关联或 Session 持久化语义。升级时待处理消息和 inbox archive 会随 inbox 重置而丢失；reset 文档已说明备份方式和多进程版本约束。

## 替代方案

- 在 Rust 内部使用明确字段、对外继续保留泛称 JSON 字段：拒绝，会让持久协议和 renderer 继续形成第二套术语。
- 同时读写旧字段 alias：拒绝，旧 inbox 格式属于本地跨进程协议，混合版本会产生难以发现的忽略/重复处理；项目按文档重置本地 inbox。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`
- IPC contract checkers 与 ADR index checker
- `git diff --check`

## 回滚

回滚代码前需先确认本地 inbox 内容是否仍是旧格式；新格式 JSONL 被旧二进制忽略。若恢复旧二进制，先退出所有 Haven 进程并重置 inbox，避免混合格式记录。
