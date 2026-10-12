# 1032：统一 RequestKind 的语义字段名

## 状态

已采纳并实施（2026-10-12）。

## 背景

`LlmUsageRecord`、Agent usage event 与 media-plan event 用 `role` 保存 Common `RequestKind`。这与 transcript 消息的 canonical `role` 不同，也让调用用途被误读成说话方角色。`LlmUsageRecord` 同时作为 SQLite 投影模型、resume DTO 和 `usage_recorded` durable payload。

## 决定

- Memory、Agent、App 与 UI wire/renderer 字段统一使用 `request_kind` / `requestKind`。
- 新写入的 `usage_recorded` payload 使用 `request_kind`；Memory 只为旧 payload 的 `role` 保留反序列化 alias。SQLite 物理列 `llm_usage.role` 保持不变，并由 Memory 显式映射到 `LlmUsageRecord.request_kind`。
- 消息 speaker `role`、provider wire `role` 与 `RequestKind` 保持不同语义；本决定不重命名前两者。

## 影响

数据库 schema 和物理列不变，无需重置。durable usage event 的新字段名变为 `request_kind`；历史 `role` payload 可继续 replay，新写入只序列化 canonical 字段。UI resume parser 接受生成的 `request_kind`，live Agent event 与 media-plan renderer 使用 `requestKind`。

## 验证

- `cargo test --locked -p haven-memory usage`
- `cargo test --locked -p haven-agent media_tool_usage_flows_to_event_and_database`
- App usage 与 media-plan event DTO focused tests
- UI focused tests、`corepack pnpm run check`
- `scripts/check-ipc-contracts.ps1`、`scripts/check-ipc-events.ps1`

## 回滚

若回滚命名，必须继续保留对历史 `request_kind` usage payload 的读取，或明确限定只支持由当前版本新写出的事件；不得把 SQLite 物理列 `role` 当成领域模型字段名的依据。
