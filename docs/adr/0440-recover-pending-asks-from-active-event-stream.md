# ADR 0440：从活动事件流恢复未回答的 Ask

- 状态：Accepted
- 日期：2026-10-03
- 关联：[ADR 0502](0502-session-actor-event-sourced-state.md)、ADR 0416、0424、0430

## 背景

Ask 的问题结果先作为 `transcript` 事件与消息/步骤投影提交；待回答状态随后由
`interaction_requested` domain event 单独提交。进程若在这两个事务之间意外退出，问题正文仍可恢复，
但事件流里没有 pending interaction。聊天页因此把 Ask 卡片当作已结束；actor 也无法识别后续输入为回答。

## 决定

1. Agent 统一从活动 `session_events` 重建交互状态。Ask `tool_result` 建立一个以 `step_id` 为稳定身份的
   pending 请求；之后的显式 `interaction_requested` 按其 request id 和 correlation ids 替换该恢复项。
2. `UserInject` 的 `source=answer` 和 `interaction_cleared` 都关闭 pending Ask。普通 `FollowUp`、消息文本相同与否、
   以及 messages/session_steps 投影都不参与回答判断。
3. actor 启动和 session resume IPC 共用 Agent 的 replay reducer，使交互门控和 UI hydration 得到相同结果。
   SessionStore 的 resume read model 暴露活动事件流供该 reducer 使用。
4. 不变更持久格式、数据库 schema、UI wire DTO 或 Ask 正文所有权；无需重置数据。

## 替代方案

- 仅在 UI 把没有 request 的最后一个 Ask 卡片显示为待回答，会让后端仍把用户输入当普通补充，且 actor 无法恢复 ask gate。
- 从消息内容匹配回答会重新引入不可靠去重，也无法区分相同文本的普通补充。
- 把生命周期请求和 transcript 重新设计为单个事务涉及并行工具结果提交及 UI 发布顺序，超出本次恢复切片。

## 影响

活动事件流同时提供 Ask 内容与生命周期恢复所需的身份/顺序事实。明确的 Answer 注入或清理事件仍是终态权威；
普通消息、模型生成内容和物化投影不会让 Ask 自动变成已回答。

## 验证

回归验收需覆盖：Ask 结果已提交而 `interaction_requested` 尚未提交时重载仍返回 pending；显式 grouped ask 请求替换
恢复项；Answer 注入和 clear 事件关闭 pending；普通 follow-up 不关闭 Ask。按项目门禁验证 Agent、SessionStore、
resume IPC 与 UI hydration。
