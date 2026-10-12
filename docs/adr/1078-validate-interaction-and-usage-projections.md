# ADR 1078：校验交互展示字段与单次用量语义

## 状态

已接受（2026-10-13）

## 背景

生成的 IPC wire shape 描述字段是否可选，但交互 DTO 的展示字段实际由 `kind` 和 `owner` 决定；`Option` 展示字段在 wire 上保持可选，renderer mapper 再执行对应分支的领域必填校验。此前 mapper 接受缺失的 Confirm/ScheduledConfirm 展示内容，UI 会显示默认文案；Session resume 对 Ask 只要求同 ID 的执行步骤存在，没有要求 transcript 中存在 UI 读取的问题消息。`agent:usage` 则独立校验 `cost_usd` 与 `has_cost`，没有校验它们是否描述同一次调用的同一事实。

## 决定

- Resume Ask 的 `request_id` 必须同时是 canonical `step-*`，并且在 transcript `messages[].message_id` 中存在。缺少对应问题消息的恢复响应按身份错误拒绝。
- Confirm 必须有非空 `tool_name`、`summary`、有效 `risk_level` 与 `tool_index`。Session Confirm 还必须关联 canonical `invocation_step_id`；AppCommand Confirm 不得携带 invocation step 或 provider tool-call ID。Confirm 的 `permission_key` 和 receipt 继续可选。
- ScheduledConfirm 必须有非空 `tool_name`、`summary`、`permission_key`；不要求 Confirm 专属的风险、步骤和工具索引字段。
- UI 的 `InteractionRequest` 按这些 kind/owner 分支表达必需字段，mapper 在把未信任 wire payload 投影进 renderer state 前执行同等检查。
- live 单次用量要求 `has_cost === (cost_usd !== null)`。不将该关系套用到 `cumulative_cost_usd`，累计值可能包含之前调用的费用。

## 替代方案

- 继续让 UI 用通用文案补齐缺失展示字段：不采纳，因为它会掩盖 Rust producer 与 renderer contract 之间的漂移。
- 只按步骤 ID 恢复 Ask：不采纳，因为 UI 还需要按 request ID 查找 canonical 问题文本。
- 用累计成本验证单次 `has_cost`：不采纳，因为累计费用与当前调用是否有价格不是同一概念。

## 影响

未改变 Rust IPC serialization、数据库或持久化格式。缺失必要展示数据的 live 交互会被 mapper 拒绝；缺失 Ask transcript 行的 resume 会失败关闭。正常 AppCommand 确认不携带 invocation step，Confirm 的 permission receipt 仍为可选。

## 验证

- UI contract tests 覆盖 Ask 步骤/消息双重链接、Confirm 与 ScheduledConfirm 的字段要求、AppCommand 特例，以及单次用量成本一致性。
- `corepack pnpm run check` 与 `corepack pnpm run test:run` 通过。

## 回滚与数据重置

不涉及数据库或持久格式，无需重置。若放宽字段要求，必须同步修改 Rust producer 语义、renderer 类型、resume 读取路径和对应 contract tests。
