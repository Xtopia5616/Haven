# ADR 0629：统一 UI chat 提交附件类型 owner

## 状态

已采纳并实施。

## 背景

Chat 附件从 `InputRouter` 经过页面回调、`ChatSessionController` 到 `submitTranscript`。这些位置分别声明了 image/file shape，提交协调器还内联了等价匿名字段类型；类型重复使不同边界可能悄悄偏离。`InputRouter` 的文件 `size` 只供待发送预览显示，历史消息 renderer 使用可缺字段的形状以处理持久旧消息和部分媒体数据。

## 决定

1. 提交输入实体由 `chatAttachmentTypes.ts` 唯一拥有：`ChatImageAttachment { media_type, data }` 与 `ChatFileAttachment { media_type, data, filename }`。
2. Composer 暂存文件使用私有 `PendingChatFileAttachment` 扩展提交类型并增加 UI 预览所需的 `size`。
3. 页面、`ChatSessionController` 与 `submitTranscript` 直接引用共享类型，不再局部重建同一输入 shape。
4. `ChatBubbleAttachment` 保持独立，因为它是历史消息 renderer 的宽松 view shape，允许 `media_type`、`data` 缺失并支持路径字段。

## 替代方案

- 把 `size` 加入提交类型：拒绝，它只服务本地预览，不进入 transcript 请求。
- 让历史 renderer 也直接引用提交类型：拒绝，renderer 的可选字段和路径兼容表示另一阶段的 view contract。
- 只删掉 `submitTranscript` 的内联类型：拒绝，其它组件和 controller 仍会分别定义提交 shape。

## 影响与验证

- 这是 UI 内部 TypeScript 类型 owner 收敛，没有 IPC payload 或生成 contract 变化。
- 命名审计 §5.7 继续覆盖 Svelte/TypeScript；组件 props、事件和其余 contracts 尚待逐域审计。
- 验证：`pnpm run check`、`pnpm run test:run`、`pnpm run build`、ADR 索引及 staged diff 检查。

## 回滚

恢复原本地附件类型与 `submitTranscript` 匿名字段声明，并将各调用方改回对应声明；无需 IPC 或数据迁移。
