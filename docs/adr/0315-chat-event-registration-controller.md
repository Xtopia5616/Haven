# ADR 0315：聊天页事件注册组合 controller

状态：已采纳（2026-09-25）

## 背景

ADR 0313 将会话命令和恢复编排迁出 `+page.svelte`，ADR 0314 将 `SessionReducer` 内部按职责拆分。聊天页仍在 `onMount` 中拼接 session、app、agent 与 usage handler map，并直接管理异步注册句柄，事件生命周期继续挤占路由职责。

## 决定

- 新增无 Svelte / DOM 依赖的 `ui/src/lib/chatEventController.ts`，只拥有聊天页 handler map 组合及注册生命周期。
- Controller 通过显式 typed dependencies 接收 session reducer dispatch/getter、session error/ask/stream 清理、会话列表刷新与终态缓存回收、chunk handler/flush，以及 hotkey setter 和 model refresh/跳过标记回调；不使用 `any` 或页面 facade 打包依赖。
- `register()` 幂等并返回 listener-ready promise；`dispose()` 幂等，并在注册尚未 ready 时立即释放注册句柄。`events.ts` 的 `registerListeners` 继续负责 late subscription cleanup。
- `events.ts` 仍是唯一 wire mapping 和通用 listener registration primitive。Controller 不改事件名、payload、mapper 或 handler 业务语义，只组合既有 typed event adapters 与 `chat*EventHandlers`。
- 页面创建 controller、等待 `register()` 后才开始 settings/load/restore，并在 `onDestroy` 调用 `dispose()`。页面继续负责 `dead` 检查、settings 与会话加载/自动恢复、model sync、ask/input、reducer、滚动；flush chunks、performance provider、scheduler 和 window click 等 teardown 也留在页面。

## 替代方案

- 继续在页面拼接 handler map 和注册器：保留了已有职责混杂，拒绝。
- 将 wire mapping 一并移入 controller：会产生第二个事件映射权威点，拒绝。
- 使用 `any` 或 `PageFacade` 隐藏页面依赖：无法清楚审查状态和生命周期边界，拒绝。

## 影响与验证

- 纯 Vitest 通过注入 registration port 覆盖 session/app/agent/usage 通道、ready 等待、dispose 幂等和 dispose-before-ready 竞态；不启动真实 Tauri。
- 事件及命令 IPC 名称、payload、mapper、Rust DTO 和持久化契约均不变，无需数据重置。
- Phase 8 后续仍需完成 Rust DTO 到 TypeScript contract/mapper 的生成收口、model operation 归属、旧手写 contract 镜像清理，以及 session/selector subscriptions。

## 回滚

回滚本切片即可恢复页面中的 handler map 组合与注册生命周期；移除本 ADR、索引和架构/路线图说明。无需恢复数据或迁移 IPC contract。
