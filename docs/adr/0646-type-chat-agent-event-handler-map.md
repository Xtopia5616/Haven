# ADR 0646：类型化 chat Agent event handler map

## 状态

已采纳并实施。

## 背景

Agent event contract 通过 `AgentEventPayloadMap` 将 channel 与 camelCase payload 配对，但 `createChatAgentEventHandlers` 返回 `Record<string, (event: any) => void>`，丢掉了这一 channel/payload 约束；chunk handler 还用 `(...args: any[]) => any`。同一模块的 `ChatEventControllerDependencies.chunkHandler` 已由 `StreamEventAggregator['chunkHandler']` 拥有精确签名。事件映射 adapter 内部另有 `AgentListenerMap`，但它是私有类型，无法由 event-to-transcript controller 复用。

## 决定

1. 将 event adapter 的 channel-indexed map 命名并导出为 `AgentEventListenerMap`，直接从 `AgentEventName` 与 `AgentEventPayloadMap` 映射 payload 参数。
2. `createChatAgentEventHandlers` 复用 `StreamEventAggregator['chunkHandler']`，返回对象使用 `satisfies AgentEventListenerMap`，从而保留局部处理器的具体必有 key，同时校验每个 channel 参数类型。
3. 不为 chat 重复声明另一份 channel/payload map，也不将该局部 subset 扩大为所有 Agent event channels。

## 替代方案

- 继续返回 `Record<string, (event: any) => void>`：拒绝。它绕开了已有的 generated payload contract，并允许错误 channel 或 payload 越过类型检查。
- 在 chat controller 再手写 handler map：拒绝。它会复制 `AgentEventPayloadMap` 与 adapter map 的 owner。
- 直接返回 `AgentEventListenerMap`：拒绝。该类型的所有 key 可选，会让当前 tests 与调用方对明确存在的 chat handler 失去非空保证；`satisfies` 只校验，不扩大返回类型。

## 影响与验证

仅收窄 UI 内部 event adapter 与 transformation controller 的函数类型；channel 名、payload mapping、注册时序和 reducer action 不变。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

如需回滚，恢复局部 `any` handler map 和非导出 adapter map，并同步撤回命名规范、路线图和 ADR 索引；无 IPC 或持久化变化。
