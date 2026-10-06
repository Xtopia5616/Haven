# ADR 0559：UI 会话时间线复用 SessionMessage 并统一 Session 命名

## 状态

已采纳并实施；UI check、全量测试与生产构建通过。

## 背景

`sessionReducer` 已拥有当前前端运行消息的唯一形状 `SessionMessage`。时间线模块又定义了宽松的 `ConversationMessage`，字段重复且附有开放索引签名；`+page.svelte` 实际传入的正是当前 session 的 reducer 消息。时间线与活动分组、空状态组件均只服务该 session UI，却沿用 `Conversation*` 模块和类型名。另有 `ChatBubble` 的附件展示输入接受可选路径，宽于 reducer 附件字段，属于单组件的 view 输入。

## 决定

1. 移除时间线中重复的 `ConversationMessage` 结构，算法直接接收 `SessionMessage`。
2. 将模块、组件、测试与作用域型输出改为 `sessionTimeline`、`SessionTimeline`、`SessionActivityGroup`、`SessionEmptyState`、`SessionTimelineItem` 等名称；消息右键请求命名为 `SessionMessageContextMenuRequest`。
3. 将 transcript 分组返回类型收窄为 `SessionTranscriptItem`，与可额外包含 ToolRun 卡片的最终 `SessionTimelineItem` 区分。
4. `ChatBubbleAttachment` 保持组件本地展示类型：它允许可选 `path`，而 reducer 附件不带此 view 字段。自然语言对话、历史文本与模型上下文中的 `conversation` 继续按含义使用。
5. 不改变消息分组、ToolRun 锚定、交互回调、wire/IPC、数据库字段或用户可见行为。

## 替代方案

- 保留开放的 `ConversationMessage` 并与 `SessionMessage` 并行：拒绝。现有唯一生产输入已经是 reducer 消息，第二结构只放宽字段约束并重复维护类型。
- 将附件展示路径加入 reducer 消息类型：拒绝。路径只属于 `ChatBubble` 的展示输入，不应扩成 session runtime 消息状态。
- 把所有“conversation”文本机械替换为 session：拒绝。自然语言交流、历史文本和模型上下文仍然是 conversation；只有此处指向会话实体的 UI owner 与符号改用 Session。

## 影响与验证

- 改动限制在 UI 内部类型、模块和组件名；不影响持久化、生成 IPC 契约或事件 payload。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、`git diff --check`。

## 回滚

恢复原 UI 类型与组件名称，并还原时间线内的 `ConversationMessage` 结构；无数据迁移或配置重置。
