# ADR 0966：校验 Agent transcript 事件身份

## 状态

已接受并实现（2026-10-11）。

## 背景

`session_events.payload` 是 Agent `TranscriptRecord` 的持久化 JSON。Memory 负责事件序号、存储与 append-only 契约；payload 字段和 replay 语义由 Agent 拥有。此前 Agent 的 seed、append 和 replay 只做 JSON 序列化/反序列化，内部 Haven 实体 ID 未统一检查。提交 UI 注释也会随 payload 持久化，但其 step、message 与 asset identity 未校验。

## 决定

- 在 Agent transcript owner 增加统一身份校验，并由 `TranscriptRecord::to_payload` 用于 seed、append 和附属媒体事件写入；resume/replay 在接受事件前执行同一校验。
- Thought 使用共享 assistant message/step 的 `step-*`；Reasoning 与可选 UserInject message、compaction episode 及带 ID 的 compacted message 使用 `msg-*`；ToolResult 使用 `step-*`。
- UserInject、MediaPlan、CompactSummary 的媒体输入、managed-file reference、projection 和 notice 中的资产身份统一要求 `asset-*`。
- Committed UI 注释校验 ToolCall/Observation 的 `step-*`、Supplement 的 `msg-*` 与 Observation assets 的 `asset-*`。Observation step 必须与同一 ToolResult record 的 `step_id` 一致；带 `message_id` 的 UserInject，其 supplement identity 必须复用该 message ID。
- Provider tool-call IDs、provider JSON 与工具参数保持不透明，不套用 Haven 实体 ID 规则。
- Replay 对畸形 transcript identity 失败关闭；提交 UI publisher 对畸形记录或 UI identity 不发布事件。

## 替代方案

- 让 Memory 解析 payload：会将 Agent transcript 语义引入 Memory，模糊现有 owner 边界。
- 只在 producer mint 时信任 ID：无法保护公开的 Agent 持久化入口和从 durable event 恢复的历史数据。

## 影响

只校验既有 JSON 字段，不改变 event type/version 或 payload shape。符合规范的历史事件保持兼容；新写入拒绝畸形 Haven ID，历史畸形记录无法 replay，UI publisher 跳过畸形行。没有 schema、配置或 IPC 变更，也无需迁移合法事件。若实际数据包含畸形 transcript，按 [release-and-reset.md](../release-and-reset.md) 的受支持恢复流程处理。

## 验证

- Unit tests 覆盖各类 transcript/message/media ID、嵌套 managed reference、UI 注释身份、写入拒绝、replay 拒绝和 provider ID 保留。
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

删除 Agent transcript 和 committed UI 的身份校验并恢复原 JSON 编解码即可；没有 schema 或有效 payload 迁移。
