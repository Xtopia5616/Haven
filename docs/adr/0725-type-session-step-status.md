# ADR 0725：Session step 状态复用闭合生命周期类型

## 状态

已采纳并实施。

## 背景

`session_steps.status` 的 schema CHECK 已限定 `pending`、`running`、`completed`、`failed`、`cancelled`、`unknown` 六个值，但 Memory `SessionStep.status` 与 Agent live `StepInfo.status` 都用自由 `String`。该字段穿过 resume command 生成到 UI；`resumeMessages` 再接收任意字符串并自行筛选失败、取消和 unknown 状态。

Agent/Memory 已有 `ToolStepOutcome`，它只接受工具步骤的终态写入意图；它不能表达 pending、running，也不是完整行状态。它与状态字段的相同值由手写字符串衔接，导致存储、live snapshot、resume contract 和 UI 分别定义开放类型。

## 决定

- 在 Common 生命周期词汇中定义 `SessionStepStatus`，覆盖六个既有存储值并按 snake_case 序列化。
- Memory 的持久 `SessionStep` 和 Agent 的 live `StepInfo` 均使用 `SessionStepStatus`；generated IPC 与 UI resume helper 引用同一闭合类型。
- Memory 从 SQLite 读取状态时严格解析；CHECK 之外的损坏值返回 SQL conversion error，不作为自由字符串透传。
- `ToolStepOutcome` 保留为只表达 finish 操作终态意图的输入类型，并映射到 `SessionStepStatus`；它不承担行状态或 UI wire owner。
- 状态 SQL 参数从 `SessionStepStatus` 生成。SQLite 列名、合法文本值和 schema 不变。

## 替代方案

- 只把 UI 字符串改成手写 union：拒绝。Rust Memory 与 Agent 两条 DTO 路径仍保留自由字符串。
- 删除 `ToolStepOutcome` 并允许 `finish_tool_step` 接收任意 `SessionStepStatus`：拒绝。那会允许将 pending/running 作为终态输入，扩大完成 API 的值域。
- 为 enum 与 SQLite 字符串修改 schema：拒绝。现有 CHECK 已完整限制该词汇，enum 可在读取边界严格验证，无需数据迁移。

## 影响与验证

`SessionInfo.steps[].status` 与 session resume `steps[].status` 的 TypeScript 类型从 `string` 收窄为生成的 `SessionStepStatus`。合法存储值和 wire JSON 不变；CHECK 被绕过或存储损坏时，读取失败。无需重置数据库或兼容旧 IPC 客户端。

验证：Common enum 序列化测试、Memory 非法状态读回归测试、Rust workspace fmt/check/Clippy/tests、UI check/tests/build、IPC 生成与漂移检查、事件检查、ADR 索引和 diff checks。

## 回滚

恢复 `SessionStep.status` 与 `StepInfo.status` 为字符串，并恢复 resume helper 的开放字符串输入。SQLite 合法状态文本保持原值，无数据回滚或迁移。
