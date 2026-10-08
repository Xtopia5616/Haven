# ADR 0766：Session reducer 复用生成等待原因类型

## 状态

已采纳并实施。

## 背景

运行会话列表 DTO 的 `waiting_reason` 使用 generated `SessionWaitingReason`，`session:lifecycle` mapper 也只输出经值域验证的 `SessionWaitingReason | null`。Session reducer 的 `SessionSummary.waitingReason` 却声明为 `unknown`，`status-updated` 与 `run-ended` actions 则以 `string | null` 重新开放该字段。

## 决定

- reducer `SessionSummary.waitingReason`、`session/status-updated.waitingReason` 与 `session/run-ended.waitingReason` 使用 generated `SessionWaitingReason | null`。
- runtime session list 的缺失值仍在 startup projection 中规范化为 `null`；lifecycle mapper 对显式未知值仍拒绝该事件。
- `sessionWaitingReason` utility 继续接受 unknown-shaped session 输入，并在消费边界验证 generated 值域，未知/缺失值映射为 `null`。它是容错读取 helper，不是 reducer state 的第二个类型 owner。

## 影响与验证

只收窄 UI reducer 状态与 action 的静态契约；event、command、数据库和 UI 展示行为不变。固定 Node 24.20.0 下的 UI check 与完整 test suite 通过。

## 回滚

将上述 reducer 字段类型恢复为 `unknown` / `string | null` 即可；无 IPC、配置或数据库迁移。
