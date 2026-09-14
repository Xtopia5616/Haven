# ADR 0154：聊天页会话状态使用 SessionReducer

- 状态：accepted
- 日期：2026-09-14
- 范围：`ui/src/routes/+page.svelte`、聊天会话事件适配与 UI 会话状态

## 背景

聊天路由页同时承载会话列表、当前会话选择、错误恢复、事件监听和消息/流式副作用。会话状态的迁移散落在多个 handler 和异步回调中，新增行为容易在新建、切换、恢复和错误重试之间产生竞态。`sessionMessages`、用量和流式聚合已经有独立边界；会话选择与错误态也需要一个可测试的唯一状态入口。

## 决定

1. 以 `ui/src/lib/sessionReducer.ts` 的纯 `reduceSession` 和 `SessionReducer` 作为会话列表、当前会话和错误态的唯一状态迁移入口。
2. `session:created` 的 fresh-start/draft-adoption 选择规则、列表刷新时保留活动错误会话、切换时清理错误和 busy 恢复错误均由 reducer 决定，并用单元测试覆盖。
3. `chatSessionEventHandlers.ts` 只负责事件 DTO 的顺序和副作用：消息/流式清理、ask 清理、错误原因缓存、终态回收和列表刷新；它通过 dispatch 触发会话状态变化，不直接写路由页状态。
4. `+page.svelte` 通过一个 `dispatchSession` 适配器同步 reducer 状态到现有 `sessionStore` 与 `activeSessionIdStore`，保留 Svelte 响应式展示和跨页面 store 契约。
5. `ModelSettings.svelte` 本轮只补组件测试作为后续 discovery/Provider CRUD 拆分的安全网；不在没有组件行为基线时引入新的设置组件边界。

## 替代方案

继续在路由页增加会话 handler，或让每个事件 handler 直接修改 `activeSessionId`/错误变量。前者继续扩大热点，后者无法集中表达迁移不变量，也难以对异步竞态建立纯单元测试。

## 影响

- 会话状态迁移变为纯函数，可独立验证；消息、用量、流式和 IPC 契约不变。
- `+page.svelte` 仍保留 UI 编排、副作用调用和滚动生命周期；后续可继续把 session command orchestration 迁移到 reducer/controller 边界。
- Provider discovery 与 CRUD 仍暂时共存于 `ModelSettings.svelte`，但已有 discovery、保存脱敏和删除解绑的组件测试作为拆分前基线。

## 验证

- `corepack pnpm run check`
- `corepack pnpm exec vitest run src/lib/sessionReducer.test.ts src/lib/chatSessionEventHandlers.test.ts src/lib/views/ModelSettings.test.ts`

## 回滚与重置

这是前端内部状态重构，不改变数据库、配置、IPC 或持久化快照；回滚代码即可，无需用户数据重置。
