# ADR 0555：明确 UI session runtime 与录音 overlay 状态 owner

## 状态

已采纳并实施；UI 类型检查、测试与生产构建均通过。

## 背景

`runtimeStateStore.ts` 同时承载 ReAct 执行阶段、当前会话的展示状态标签和 `RecordingOverlayState` 类型。前两者由 `+page.svelte` 与 `+layout.svelte` 共享，但表示不同的 session 运行视图：ReAct phase 带来源 `sessionId` 并供提交/执行 UI 判断；状态标签仅为当前选中 session 提供 shell 展示。录音 overlay 的实际状态则完全由 `recordingOverlayController` 私有 writable 管理，通用 store 文件只定义了其类型。

这一布局使模块名没有说明状态作用域，类型也与其实际 owner 分离。`activeConversationStatusStore` 和 `WorkspaceStatus.conversationStatus` 还把 Haven 统一称作“会话”的实体称作 conversation。

## 决定

1. 将 `runtimeStateStore.ts` 重命名为 `sessionRuntimeStore.ts`，表明其包含 session ReAct phase 与选中 session 展示状态。
2. 将 `activeConversationStatusStore`、页面展示变量和 `WorkspaceStatus` prop 统一命名为 `activeSessionStatusLabel`（store 名为 `activeSessionStatusLabelStore`）；页面已有的 `activeSessionStatus` 保留表示后端原始状态。
3. 在 `recordingOverlayController.ts` 定义并导出 `RecordingOverlayState`，AppShell 与 layout 从实际 owner 导入该类型。
4. 保留 ReAct 执行阶段和选中 session 状态标签两个独立 store：前者用于带来源 session 的执行状态判断，后者是当前选中 session 的本地化展示值，作用域、字段和消费者不同；录音 overlay 仍由自己的 controller 管理。
5. 同步更新所有 UI 调用点、测试和路线图；不改变状态转移、订阅时机、页面展示或 wire/持久化字段。

## 替代方案

- 将三种状态合并为一个全局 runtime store：拒绝。它们的 owner 和生命周期不同，合并会让选中会话视图与录音生命周期共享无关状态空间。
- 保留 `runtimeStateStore` 并只移动类型：拒绝。模块剩余内容都限定于 session runtime，旧名称仍缺少作用域。
- 把录音类型移入独立 contracts 模块：暂不采用。该类型只描述 controller 提供给 AppShell 的本地 overlay view，不是 Tauri wire contract；由 controller 导出能直接指明 owner。

## 影响与验证

- 仅重命名 UI 内部模块/状态引用并移动类型声明，不改变 Rust、IPC、事件、配置、数据库或持久数据。
- 验证 UI Svelte check、测试和 production build；检查旧模块名及 `activeConversationStatus` / `conversationStatus` 标识不再留在活动 UI 源码中。

## 回滚

恢复 `runtimeStateStore.ts` 与旧状态标识，将 `RecordingOverlayState` 声明重新放回该模块并恢复调用点导入；无需数据或配置迁移。
