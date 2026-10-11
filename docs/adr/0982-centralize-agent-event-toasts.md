# ADR 0982：Agent 事件 toast 由应用布局集中处理

## 状态

已采纳并实施。

## 背景

通知规范规定事件驱动的应用内 toast 由 `+layout.svelte` 统一处理，页面 controller 只负责页面状态投影。Agent compaction 与 media-plan toast 却由 chat route 的 `chatUsageEventHandlers` 直接写 `notificationStore`，因此通知生命周期依赖页面监听器。

## 决定

1. 将 `agent:compaction` 和 `agent:media_plan` 的 toast 监听迁入 `+layout` 的 Agent event listener 集合。
2. Compaction/media-plan 文案与级别通过纯格式化函数生成；只有 layout handler 调用 `addNotification`。
3. Chat page controller 不再订阅 compaction；它保留 `agent:media_plan` 对 `mediaPlanStore` 的 session 状态投影，`agent:usage` 继续更新当前会话 usage。
4. 保持现有文案、级别、时长、事件 payload 和通知配置行为。

## 替代方案

- 页面和布局同时订阅并让页面继续投影 toast：拒绝；会保留第二个通知入口，并在页面生命周期变化时造成重复或漏通知风险。
- 将媒体计划 store 投影也移入布局：暂不采用；store 属于 chat 页面消费状态，本次只调整 toast 的全局生命周期 owner。

## 影响与验证

- Agent compaction 与 media-plan event toast 生命周期由全局布局持有；media-plan 状态仍按 session 投影给 Chat。
- 单测覆盖两类 toast 的文案/级别/时长，并确认 chat usage handler 不再发通知。
- IPC、持久化和通知配置不变。

## 回滚

恢复 chat usage handler 的 compaction/media-plan toast 并移除 layout 监听与格式化函数；同步回退通知规范、路线图和 ADR 索引。无需数据迁移。
