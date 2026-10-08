# ADR 0767：SessionSummary 使用显式 UI projection

## 状态

已采纳并实施。

## 背景

Session UI reducer 同时接收三种 generated DTO 的投影：runtime `SessionInfo` 使用 `input` 与 `waiting_reason`，persisted `SessionRecordDto` / `SessionHistoryRow` 使用 `input_text`。`SessionSummary` 曾用 `[key: string]: unknown` 接收任意 shape；startup 与 lineage mapper 整行 spread wire DTO，`SessionToolbar` 和 switcher 又直接读取 snake_case 字段。

实际 UI consumers 只需要 `id`、`status`、`title`、`input`、`inputText` 与 `waitingReason`。开放索引既掩盖字段来源差异，也让 Rust wire 命名越过 UI mapper 边界。

## 决定

- `SessionSummary` 改为显式 renderer shape，移除开放索引签名。
- runtime session list mapper 只投影 `id/status/input/title/waitingReason`；history 与 lineage mapper 将 `input_text` 转成 `inputText`，title fallback 行为保持不变。
- retained error row 用 camelCase `inputText`，SessionToolbar 与 session switcher 只读 renderer 字段。
- 生成的 command/event DTO 继续保留 Rust snake_case；转换仅发生在 startup、history/lineage view 映射边界。

## 影响与验证

Reducer 不再持有 runtime DTO 的未消费 `summary`、steps、时间戳等字段，也不再携带 `waiting_reason` / `input_text` wire key。会话 ID、状态、标题 fallback、history 选择与 lineage 展示行为不变。固定 Node 24.20.0 下 UI check 与完整 test suite 通过。

## 回滚

恢复 SessionSummary 的索引签名，并将三处 mapper 改回 DTO spread 即可；无 IPC、数据库或持久化迁移。
