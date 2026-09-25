# ADR 0322：SessionReducer 相等性门控 selector store

- 状态：已采纳（2026-09-25）
- 范围：聊天页与应用 shell 对 `SessionReducerState` 的响应式读取
- 关联：[ADR 0314](0314-session-reducer-internal-modules.md)、[ADR 0313](0313-chat-controller-session-orchestration.md)

## 背景

ADR 0314 保留一个 `sessionStateStore`，但 `+page.svelte` 与 `+layout.svelte` 都把它的每次完整值赋给 `$state`。Agent stream batch 会替换 reducer root，即使页面关心的 sessions、active session、usage、interaction 或 error/termination 切片没有改变，也会让两个路由组件收到完整状态并重新检查派生依赖。

基线审查确认：页面读取 sessions、active session ID、active transcript、ask/confirm interactions、活动会话 token stats 与 LLM usage、error 和 termination；布局读取 sessions、active session ID 和 confirm interactions。事件处理器已有 `SessionReducer.getState()` / dispatch 路径，不需要 root store 镜像。

## 决定

- 保留 `SessionReducer`、`reduceSession`、`sessionStateStore` 和 `appSessionReducer` 为既有唯一 reducer 与 state owner。新增只读 `createSessionSelectorStore`，只订阅 `sessionStateStore`，缓存一个当前 selector 结果，并以 `Object.is` 门控通知。它不复制 reducer state，不持有第二份权威状态，也不按内容去重。
- Selector store 在第一个 listener 到来时订阅 root，在最后一个 listener 离开时释放 root subscription；多个 listener 共用该 selector 的一次 root subscription。Svelte `$effect` 返回 `syncStore` 的 unsubscribe，以维持组件生命周期清理。
- 页面改为 selector 读取 sessions、active session ID、active session messages、active-session token stats/LLM usage、interactions、error 与 termination。活动消息 selector 跟随 reducer 当前 active ID（无活动会话时读 draft），缺失消息共用稳定空数组。布局改为 selector 读取 sessions、active ID 与 interactions。
- 活动 transcript 的 ask 展示继续由 `projectChatVisibleMessages` 处理；messages 或 interactions 切片变化时更新，其他 root 更新不会通知对应 selector。selector 保持既有引用，因此没有 stream 消息变化时不重新投影。
- dispatch、`getState()`、reducer 转换、状态结构、事件顺序和每次有效 reducer dispatch 的 root store 通知都不变。selector 只减少消费方对无关 root 更新的通知。
- 复杂 view state、ask/input 决策、启动恢复编排及 replay 继续走页面/controller 与 reducer 现有路径；本 ADR 不把它们改造成 selector-owned 状态，也不改变其生命周期。

## 替代方案

- 增加第二个 session store 或复制/缓存完整 reducer state：会引入新的 owner 与同步问题，拒绝。
- 深比较消息或整个 state：每个 stream batch 都会扫描内容，且会引入内容去重语义，拒绝。
- 让 `sessionStateStore` 本身忽略相同 root：Agent stream 会真实改变 transcript/replay，仍需 root 更新，不能解决消费方无关派生更新，拒绝。

## 影响与验证

- selector store 仅缓存单个当前选择结果；没有跨 session 或无界缓存。活动会话切换时根据新的 active ID 选取对应数组，缺失 session 稳定返回空值。
- 不涉及 Rust DTO、IPC、持久化、event mapping 或 reducer transition；无需数据迁移或重置。
- 单测验证无关 action 不通知、目标 slice 更新通知一次、active session message 引用切换、相同引用/稳定空值不通知，以及最后一个 listener 离开时释放 root subscription。现有 chat-visible-message ask/interaction 投影回归继续覆盖展示结果。
- 验收：`cd ui; corepack pnpm run check`、`cd ui; corepack pnpm run test:run`、`cd ui; corepack pnpm run build`、`cargo check --workspace --locked`、`git diff --cached --check`。

## 回滚

回滚本提交即可恢复页面与布局的完整 root 镜像，并删除 selector store 与其测试/文档；不涉及持久化、IPC 或用户数据。
