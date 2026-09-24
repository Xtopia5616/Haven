# ADR 0313：聊天页会话编排收口到 ChatController

状态：已采纳（2026-09-25）

## 背景

`ui/src/routes/+page.svelte` 同时承载视图状态、输入交互、事件监听和会话命令编排。`SessionReducer` 已是会话运行态的唯一 owner，`continueSession.ts` 与 `resumeMessages.ts` 也已有纯策略边界，但回退、切换、继续、结束、中断和提交仍由页面直接串接 IPC 与 reducer action，难以脱离组件单独验证。

## 决定

- 新增 typed `ui/src/lib/chatController.ts`，负责 `get_session_for_resume` 读取与消息重建、pending interaction id 保留、会话切换与终态内存回收、rollback、continue、end/interrupt，以及 `submitTranscript` 的页面级提交编排。
- Controller 通过明确依赖接收 invoke、submit、reducer 读取与 dispatch、active session 和 session snapshot getter、通知/错误报告、草稿 setter、session refresh、step block 清理及 fresh-session intent callbacks；不持有 Svelte `$state`、DOM 或组件实例。
- Continue/resume 的选择策略继续由 `continueSession.ts` 和 `resumeMessages.ts` 提供。Controller 只按原顺序执行命令、权威 reload、replay reset、follow-up 与 session refresh。
- `+page.svelte` 保留 dialog/loading/menu/scroll 状态、输入路由与 ask 交互决策、event listener 注册、model sync、resume target/auto-restore 与显式新会话入口；resume-target/event 路径需要清理终态会话缓存时也转交 controller。页面 handlers 只读取 UI 参数并转交 controller。
- Tauri command、request/response 和 event 契约不变；提交继续委托给已有 `submitTranscript`，保留其 per-session in-flight、duplicate join 与排队规则。

## 替代方案

- 继续把流程留在 `+page.svelte`：会让纯 reducer 和策略测试无法覆盖命令编排的顺序及失败保护，拒绝。
- 将组件依赖整体包装成 `PageFacade` 或 `any`：会隐藏状态所有权和边界，拒绝。
- 将 ask controller、事件监听、model sync 或 reducer 一并迁移：这些职责拥有独立生命周期，本切片不扩大边界。

## 影响与验证

- 新增 Vitest 覆盖 user/non-user rollback、continue 的两种 strategy、权威 reload 中 pending interaction 保留、创建 session 后选择、end/interrupt 失败保护及 continue 重复请求保护。
- 验证命令：`cd ui; corepack pnpm run check`、`cd ui; corepack pnpm run test:run`、`git diff --check`。
- 无数据库、持久化或 IPC 契约变化，不要求数据重置。

## 回滚

回滚本提交即可恢复页面内的会话命令编排；删除本 ADR 与索引/路线图说明。无需恢复数据或迁移数据库。
