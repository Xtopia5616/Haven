# 1006：用 durable transcript 命名媒体持久化边界

## 状态

已采纳并实施。

## 背景

ReAct durable snapshot 已退役，`session_events` 是 transcript replay 与 rollback 的权威来源。但媒体归档辅助方法仍以 `for_snapshot`、`canonical_for_snapshot` 命名，注释把 event payload 描述为 snapshot；`continue_session` 的 command contract 也仍称从 saved error snapshot 恢复。这会把持久化事件与不可变时点快照混为一谈。

BranchPoint 写路径还保留 `force` 布尔参数及“forced/throttled”解释，但方法每次都无条件 append durable marker，参数被丢弃，不影响行为。

## 决定

- 将 `MediaInput::for_snapshot` 改名为 `for_durable_transcript`；把 compacted canonical root、media-marker helper 和 serde serializer 改为明确表达 compaction/durable event 职责的名称。
- 更新 App command documentation 与 Rust/UI command metadata，说明 `continue_session` 从 durable `session_events` replay，并截断 partial projection。
- 删除 `save_branch_point` 的无效 `force` 参数及调用点旧的 forced/throttled 说明。BranchPoint 每次都先 append durable marker，再更新内存索引。
- 保持既有 CompactSummary payload 字段、managed-media marker 文本、SQLite schema 与 event replay 语义不变；保留历史 marker 文本以供 replay 和媒体恢复识别。

## 影响

这是 Common crate 内部调用方和 Rust source API 的命名调整，以及 Agent 内部函数签名收口。`MediaInput` 的序列化值、transcript 事件 payload、managed asset 身份和投影行为不变。没有持久数据迁移或重置要求。旧的 ReAct snapshot 术语只保留在明确说明其已删除的文档和历史 ADR 中。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- 搜索确认当前 `for_snapshot` / `canonical_for_snapshot` 与 saved-error-snapshot 描述已清除。

## 替代方案

- 保留旧名称并靠注释解释：拒绝；名称继续暗示已退役的恢复模型。
- 保留 `force` 参数给未来节流逻辑：拒绝；未定义节流策略，且参数现在完全不影响写入。

## 回滚

可恢复旧 Rust helper 名称和 `force` 参数而不迁移数据；若回滚命名，必须继续让注释和 command metadata 准确说明 `session_events` replay 与无条件 durable marker append。
