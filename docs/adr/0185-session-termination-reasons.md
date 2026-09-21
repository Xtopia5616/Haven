# ADR 0185：会话终止原因与对话内提示

## 背景

会话错误事件已有错误文本，但显式结束、父会话级联结束和上级会话请求停止等终态只发送标题与状态。聊天页因此无法解释每次终止的来源，且显式结束后会立即清空当前时间线。

## 决定

- `AgentEvent::SessionCompleted` 增加必填的用户可见 `reason`；会话状态更新事件也携带可选 `reason`，用于显式中断等可恢复的生命周期变化。Tauri 的 `SessionLifecycleEvent` 统一保留该可选字段。
- `session:completed` 与其 `session:updated` 副发携带同一原因；错误终态继续使用 `session:error.error`，并在 `session:updated` 副发中映射到 `reason`。
- 用户点击打断时，后端将 `paused` 更新事件标记为“用户主动打断输出”；前端把 paused/completed/error 收口为同一个生命周期投影，在对话时间线使用统一的 `SessionTerminationBanner` 展示状态、原因和后续提示。提示框沿用聊天气泡的宽度、表面、圆角、边框和阴影 token，仅按状态使用语义色。
- 用户显式结束后保留当前时间线和终态框；下一条输入通过 fresh-start intent 建立新会话，新建按钮仍可立即清空并开始新的会话。

## 影响与边界

原因是本次运行中的生命周期事件展示数据，不新增数据库列或迁移；重新从历史打开已结束或暂停会话时，如果没有本次运行中的事件缓存，使用通用回退文案。错误的详细原因仍按既有错误净化边界处理。

## 验证与回滚

验证 `cargo test -p haven-app-binary --locked`、`corepack pnpm run check` 和 `corepack pnpm run test:run`。回滚时移除终态 `reason` 字段及统一提示组件即可，不涉及数据库重置。
