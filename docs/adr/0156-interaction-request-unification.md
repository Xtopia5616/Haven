# ADR 0156：统一人工交互请求生命周期

## 状态

已接受（2026-09-14）

## 背景

ask、工具安全确认和定时任务确认都会暂停执行，等待外部主体按稳定 ID 作出决定。此前它们分别由快照字段、session 状态、定时确认列表和前端 ask/confirm 状态表示，恢复、清理和 UI 投影容易出现分叉。

## 决策

- 使用 `haven_agent::InteractionRequest` 表示 ask、confirm、scheduled confirm 的共同生命周期：`Pending → Resolved | Expired | Cancelled`。
- `ReActSnapshot.interactions` 是请求的持久化来源；session 状态只使用通用 `Paused`，暂停原因由请求的 `kind` 和 `status` 表达。
- Tauri 只发送 `interaction:requested` 的 renderer-safe 投影。confirm 的原始工具参数和授权 receipt 保留在后端，resolve 仍由后端按请求 ID 校验。
- 前端使用单一 `interactionStore`。ask 卡片和确认弹窗都是该 store 的投影，不能再建立并行队列或等待字段作为状态源。
- 旧快照/旧数据库不做运行时迁移；数据库契约升至 v20，按发布重置说明重新创建。

## 不变量与验证

- 请求 ID 在事件、快照和 resolve 调用中保持稳定；重复事件按 ID 幂等覆盖。
- 所有待处理请求都能在会话恢复、rollback、结束和超时路径中被清理或恢复，且不会凭文本内容猜测回复归属。
- 后端覆盖 InteractionRequest 序列化、决策关联、恢复和 rollback 清理；前端覆盖统一事件映射、store 生命周期和 ask/confirm 投影。

## 影响与回滚

这是一次持久化和 IPC 契约变更。回滚需要恢复 v19 数据库/旧快照契约并重新启用旧事件，不能对 v20 数据库做原地降级迁移。
