# ADR 0200：为 Paused 提供派生等待原因并收口确认投影

## 状态

已接受（2026-09-22）

## 背景

`SessionStatus::Paused` 是持久化层的粗粒度生命周期状态，但实际含义可能是
用户暂停、等待 ask、工具确认、定时任务确认、后台任务或步骤预算。此前前端需要
把 session 状态、InteractionRequest 和 action registry 组合起来猜测原因；直接 UI
调用还在应用层维护独立的确认事件结构，导致它与 Agent/Scheduled confirmation
容易产生字段漂移。

## 决定

- 保留 `Paused` 作为持久化状态；新增非持久化的
  `SessionWaitingReason`，通过 session 列表和 lifecycle event 的 `waiting_reason`
  派生字段向 UI 暴露稳定的 snake_case 枚举值。
- Agent 在暂停边界记录显式原因，恢复/列表投影在缺少缓存时从
  `InteractionRequest`、scheduled confirmation 和 `ActionService` 读取原因；非暂停
  状态始终不发送该字段。
- UI 在边界把 `waiting_reason` 转为 `waitingReason`，状态标签和工作区状态直接消费
  该字段；action registry 只保留任务列表和数量等展示细节，不再决定暂停原因。
- 直接 UI 确认也先构造 `InteractionRequest::Confirm`，并复用统一的
  `project_interaction` renderer-safe 投影。应用层仍保留原始授权请求和 typed action
  作为 resolve 后端执行载荷，不把敏感参数发送到 renderer。

## 不变量与验证

- `waiting_reason` 只表示当前 `paused` 会话的可继续条件，不写入 sessions schema。
- `interaction:requested` 仍是 ask、Agent confirm、scheduled confirm 和直接 UI confirm
  的统一 renderer 事件；request id 是 resolve 的唯一关联键。
- 覆盖 Rust 生命周期/InteractionRequest 单测、UI 合约与标签单测，并运行 workspace
  check、Agent/app tests、UI type check 和单次测试。

## 影响与回滚

这是一个向后兼容的 IPC 增字段；旧前端忽略 `waiting_reason`，新前端对缺失字段回退
为通用暂停文案。回滚只需移除派生字段和 UI 消费，不涉及数据库迁移。
