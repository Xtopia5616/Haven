# ADR 1082：统一 renderer session 输入与消息身份投影

## 状态

已接受（2026-10-13）

## 背景

renderer 的 `SessionSummary.input` 与 `inputText` 都表示 session 首条用户输入；历史和 runtime list 分别填入不同字段。Resume target 的 `summary` 也承载 `sessions.input_text`，与真正的摘要概念冲突。流聚合器的 `tid`/`sid` 持有稳定 session/message identity，名称却丢失实体语义。此外，pending message owner map 将 accepted/rejected 状态留存在 session state，既无读取者也不会清除；该 owner 只在提交尚未终结、草稿可能被生命周期事件迁移时用于失败回滚。

## 决定

- `SessionSummary` 与 `SessionResumeTarget` 对初始用户输入统一使用 `inputText`；所有生产者、消费者在 renderer 边界完成映射。
- `PendingChunk` 与 reducer payload 使用 `sessionId`、`messageId`，不以 `tid`、`sid` 表达实体身份。
- reducer 将 `optimistic` 改为仅存 pending owner 的 `pendingMessageOwners`；草稿 adoption 时更新 owner，accepted/rejected 时删除映射，不保留终态副本。
- 在 reducer tests 覆盖 session clear、草稿迁移后的拒绝回滚与终态清理。

## 影响

改变 renderer 进程内状态名称，不改变 Tauri wire payload、数据库或持久化数据。提交成功或失败后不再保留额外的 message-owner map 条目。

## 验证

- Session startup、switcher、stream aggregator 与 reducer 定向 UI tests。
- `corepack pnpm run check` 与 `corepack pnpm run test:run`。

## 回滚

恢复旧 renderer state 名称时须同步所有 mapper、consumer 与测试；不得保留无生产用途的 accepted/rejected owner 条目。
