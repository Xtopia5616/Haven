# ADR 0160：UI 统一 typed SessionReducer 运行态

- 状态：accepted
- 日期：2026-09-15
- 范围：`ui/src/lib/sessionReducer.ts`、聊天事件适配、resume/提交路径与会话视图

## 背景

ADR 0154 只把会话列表、当前会话和错误态收口到 `SessionReducer`。消息、
Interaction、token usage、流式 block/sequence 以及 optimistic user bubble 仍由
多个 store、stream aggregator 和异步回调分别修改。这样 live event、resume、断线后
重放和一次提交的成功/失败会从不同入口更新同一条时间线，容易出现重复 chunk、旧
tool card 覆盖 live observation、草稿迁移丢消息或确认状态漂移。

后端现在以 `SessionEventStore` 提供 append-only durable sequence（ADR 0159），
UI 需要一个同样按稳定 ID 和 sequence 合并的运行态边界。

## 决定

1. `ui/src/lib/sessionReducer.ts` 的 `reduceSession` 和 `SessionReducer` 成为聊天
   运行态的唯一迁移入口。一个 typed state tree 同时持有会话摘要/选择、消息、
   Interaction、token/LLM usage、tool output preview、optimistic 状态和 replay
   cursors/block identity；`sessionStateStore` 只是该状态的 Svelte 订阅投影。
2. `chatAgentEventHandlers.ts`、`chatSessionEventHandlers.ts`、usage/interaction
   handlers 只把已完成边界转换成 `SessionAction` 并 dispatch。`+page.svelte`、
   `+layout.svelte`、MemoryView 和 voice/typed submit 保留 IPC、通知、滚动等副作用，
   不再直接拥有会话运行态。
3. resume、切换、rollback 后同步和继续生成都使用同一个
   `session/messages/resume-loaded` action。合并只按稳定 message ID；resume 可以
   保留仍在 streaming 的 live 项，并按显式排除 ID 丢弃继续生成前的 stale 项，禁止
   通过内容相等猜测身份。
4. `agent/chunk` 由 reducer 记录 per-message chunk sequence，`agent/action`、
   `agent/observation` 和 `agent/supplement` 记录 backend `eventSeq`；重复或落后
   的事件返回原 state。stream reset、rollback、终态清理分别清理对应 replay
   边界，不能让断线重放重新追加已经消费的输出。
5. optimistic 消息使用 add → accepted/rejected 生命周期。SessionCreated、draft
   adoption 和 persisted `message_id` 均按 ID 迁移/重命名；失败只删除同一个
   optimistic ID。保留的 `sessionMessages.ts`、`sessionUsage.ts` 兼容 API 只服务
   尚未迁移的测试/边界投影，生产聊天路径不再把它们作为会话状态源。

## 替代方案

继续让每个 handler 修改自己的 store，或者只把消息迁移到 reducer 而保留独立的
usage/replay store。两种方案都会保留多个写入时钟，无法对 resume 与 reconnect replay
建立统一的幂等规则，也无法验证 optimistic 生命周期与会话迁移是否一致。

## 影响

- live event、resume、断线重放和 optimistic 状态共享一个可测试的 typed transition
  表；组件只读取 reducer 的投影。
- `streamAggregator` 仍负责 RAF 排队和相邻 chunk 折叠，但提交的是 reducer action，
  不再直接写消息 store。
- 旧 store 导出暂时保留为只读兼容投影/测试边界，不改变 Tauri wire、数据库 schema
  或已有持久化数据。

## 验证

- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui exec vitest run src/lib/sessionReducer.test.ts src/lib/streamAggregator.test.ts src/lib/submit.test.ts src/lib/chatSessionEventHandlers.test.ts src/lib/chatUsageEventHandlers.test.ts src/lib/chatInteractionEventHandlers.test.ts`
- `corepack pnpm --dir ui run test:run`
- `corepack pnpm --dir ui run build`

## 回滚与重置

这是前端运行态重构，不改变数据库、配置、IPC 或持久化快照；回滚代码即可，无需
用户数据重置。若保留后端 durable event sequence，回滚后的 UI 仍必须继续尊重
message ID、chunk sequence 和 event sequence，不能恢复内容匹配去重。
