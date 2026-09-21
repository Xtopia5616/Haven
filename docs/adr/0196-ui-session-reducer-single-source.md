# ADR 0196：SessionReducer 成为 UI 会话运行态唯一来源

## 状态

已接受（2026-09-21）

## 背景

ADR 0160 已将消息、Interaction、用量、流式 replay 和 optimistic 状态纳入
`SessionReducer`，但迁移期的 `sessionMessagesStore`、用量 Store 与
`interactionStore` 仍被页面作为只读镜像维护；提交、问答、流式聚合和用量 handler
也保留了有 reducer / 无 reducer 两条路径。这样虽然当前行为通常一致，代码仍无法从
调用点判断会话状态的唯一来源。

## 决定

- 删除旧消息 Store 及其 helper；消息、草稿迁移、提交成功/失败和流式 sequence 全部
  通过 `SessionAction` 进入 `SessionReducer`。
- 删除旧 token stats/LLM usage Store 和交互 Store；用量恢复、live usage、Interaction
  hydrate/resolve/clear 全部由 reducer state/action 承载。纯用量格式化与 token 计算
  helper 保留为无状态函数。
- `+page.svelte` 只订阅 `sessionStateStore`，不再把 reducer state 同步到旧 Store。
- 事件 handler、问答 controller、stream aggregator 和 submit API 要求显式 reducer
  dispatcher，不再提供无 reducer 的兼容调用路径。

## 替代方案

继续保留只读镜像，或让 helper 在测试中走旧 Store。这会继续扩大状态边界，并允许
新增调用者绕过 reducer；测试版没有兼容旧内部 API 的必要。

## 影响

这是 UI 内部运行态 API 的破坏性收窄，不改变 Tauri wire、数据库 schema、持久化数据
或用户配置；无需数据重置。旧 Store 测试改为直接验证 reducer state，reducer 继续
负责消息、交互、用量、optimistic 和 replay 的唯一迁移。

## 验证

- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`
- `corepack pnpm --dir ui run build`
- `corepack pnpm --dir ui exec prettier --check`（本次变更文件）
