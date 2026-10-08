# ADR 0765：Session UI reducer 复用生成状态类型

## 状态

已采纳并实施。

## 背景

运行态 session list 由 `RuntimeSessionListResponse` 返回，状态 owner 是 generated `SessionStatus`；`session:lifecycle` 的 `created` 与 `updated` variants 分别使用 generated `SessionStatus` 和 `SessionUpdateStatus`。Session reducer 的 `SessionSummary.status`、created/status-updated actions 却接受开放 `string`。session event renderer 还用 `Extract<SessionStatus, ...>` 再声明一次 `updated` 子集。

这允许 reducer 内部状态和已验证 lifecycle event 在静态层面脱节，也让 `session/status-updated` 可以表达 completed/error，尽管该 action 只由非终态 `updated` event 生成。

## 决定

- `SessionSummary.status` 与 `session/created.status` 使用 generated `SessionStatus`。
- `session/status-updated.status` 与 `SessionLifecyclePayload.updated.status` 使用 generated `SessionUpdateStatus`；删除本地重复的 `NonTerminalSessionStatus` alias。
- `SessionRunEndStatus` 继续从 generated `SessionStatus` 提取 paused/completed/error，作为专用终态 notice/action 输入。
- `session/status-updated` 只可能收到 pending/running/paused，因此对同一 session 的既有 run-end notice 总是失效；completed/error 仍由 `session/run-ended` 承载。
- `sessionStatus.ts` 的动态字符串判断 helper 保留开放参数与未知值 fallback；它们处理不可信/历史文本，与 typed reducer state 的 owner 不同。

## 影响与验证

只收窄 UI 内部状态类型与一个不可能的 action 测试场景；IPC、数据库值、event 顺序和 reducer 的有效生产行为不变。固定 Node 24.20.0 下 UI check 与完整 test suite 通过。

## 回滚

恢复 reducer status 字段为 `string`、恢复本地 `NonTerminalSessionStatus` alias 及开放终态比较即可；无 IPC、配置或数据库迁移。
