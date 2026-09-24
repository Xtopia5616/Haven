# ADR 0314：SessionReducer 内部按职责拆分

状态：已采纳（2026-09-25）

## 背景

Phase 8 已将聊天页的异步会话编排提取到 `ChatController`（ADR 0313），但 `ui/src/lib/sessionReducer.ts` 仍同时包含会话生命周期、transcript、interaction、usage 和 Agent 流事件处理。文件难以按职责独立阅读和维护，也使变更边界不清楚。

`SessionReducer` 是会话运行态唯一 owner。当前重构只拆内部纯状态转换，不改变调用方、Tauri contract、事件顺序、状态结构或 Svelte 订阅方式。

## 决定

- 保留 `ui/src/lib/sessionReducer.ts` 作为稳定 facade：原有 `SessionReducer`、`reduceSession`、`initialSessionState`、`sessionStateStore`、`appSessionReducer`、`resumeInteractions`、`backgroundActionResultContent` 与公开类型继续从原路径导出。
- 将状态转换按职责放入 `ui/src/lib/sessionReducer/`：
  - `lifecycle.ts`：会话列表、选择、创建/删除、状态、错误、终态和标题。
  - `transcript.ts`：消息、optimistic 生命周期、草稿接管、resume 消息合并、截断、replay 清理和后台任务结果。
  - `interaction.ts`：interaction upsert、resume hydration、pending 保留、resolve 和 resume 投影归一化。
  - `usage.ts`：恢复用量、实时 token stats / 调用明细和清理。
  - `agent.ts`：chunk batch、thought/action/observation、supplement、web search 和 stream reset。
  - `state.ts`：不可变消息/replay 操作、事件序号去重历史及 stream block identity 的共享纯 helper。
  - `types.ts`：reducer state/action 与公开类型、初始 state 的唯一声明。
- 领域模块只接收显式 state/action，并共享 `state.ts` / `types.ts`；领域模块之间不互相导入，不访问页面或 store。
- Facade 的 `reduceSession` 负责 action 路由和少量跨域组合：session/messages/cleared 与全局 sessions/cleared 的 slice 清理，以及 resume-loaded 依次组合消息、interaction、usage 更新。Observable wrapper、单个 `sessionStateStore` 和 `appSessionReducer` 仍由 facade 持有。
- 继续保持 immutable state 更新和每个 reducer dispatch 一次 store/subscriber 通知。所有现有行为测试保留，并补充 resume + pending interaction + usage restore/live、stream reset + chunk sequence、error + termination 刷新的跨模块回归。

## 替代方案

- 只按行数切文件：会把耦合的状态转换拆开，边界仍不清楚，拒绝。
- 让领域模块互相调用：会形成隐式流程和依赖方向，拒绝；跨域顺序由 facade 显式组合。
- 同步调整 selector 订阅、状态 shape 或 event contract：超出本切片，且会改变既有行为或跨层边界，留待各自 Phase 8 工作项。

## 影响与验证

- 外部 reducer API、公共导入路径、状态 shape、消息/interaction/usage/replay 语义、事件 contract 与 Svelte store 订阅粒度不变。
- 不涉及数据库或持久化格式，不要求重置数据。
- 验证：`cd ui; corepack pnpm run check`、`cd ui; corepack pnpm run test:run`、`git diff --check`；`cargo check --workspace --locked` 作为跨端回归。
- Phase 8 仍需处理 Rust DTO 到 TypeScript contract 的生成、事件登记/映射收口、model 操作归属、旧镜像清理，以及后续是否引入 session/selector 订阅。

## 回滚

回滚本提交即可恢复单文件 reducer；无需恢复数据、迁移数据库或更改 IPC contract。
