# ADR 0843：按结构命名 Chat 附件输入 payload

## 状态

Accepted — 2026-10-10

## 背景

`chatAttachmentTypes.ts::ChatImageAttachment` 只有 `media_type` 与 base64 `data` 两个字段。`InputRouter` 的图像压缩流程使用它，读取普通文件的 `readAsAttachment` 也返回同一类型，再由调用方补上 `filename` 形成 `ChatFileAttachment`。因此“image”不是这个基础结构的稳定语义：它描述部分消费者的用途，却错误限制共享 payload 本身。

附件 payload 已跨 InputRouter、页面、Ask controller、Session controller 与 submit coordinator 共享。其 `media_type` / `data` 字段与 generated `MessageAttachmentInput` 相同，手写重复定义会让 Tauri 输入 contract 和 UI 输入 shape 漂移。

## 决定

- 删除 `ChatImageAttachment`，改用 `ChatAttachmentPayload` 表达 inline `media_type` + `data`。
- `ChatAttachmentPayload` 从 generated `MessageAttachmentInput` 选取字段，作为跨组件提交路径唯一的基础 payload shape。
- 保留 `ChatFileAttachment`，表示同一基础 payload 加必需的 `filename`；`PendingChatFileAttachment` 仍只为 InputRouter 本地预览增加文件大小。
- `images` 参数与图像压缩路径继续表达图像用途；泛化读取函数返回通用 payload。历史 transcript renderer 保持自己的 `ChatBubbleAttachment` view，因为它允许输入 payload 没有的路径等字段。
- 不保留旧类型名 alias。

## 替代方案

- 保留 `ChatImageAttachment` 并仅修正文档：拒绝。普通文件读取仍会返回名为 image 的类型，概念错位继续存在。
- 为 images 与 files 各复制一份 `{ media_type, data }` 定义：拒绝。它们共享同一 generated 输入字段，复制会重新引入双重 owner。
- 直接让所有 renderer 与 transcript 使用 `MessageAttachmentInput`：拒绝。历史 renderer 有独立的可选路径/显示字段和容错投影，不是 command input contract。

## 影响与验证

这是 UI 内部类型命名与引用来源调整。Tauri request、JSON 字段、图片/文件分类、压缩及提交行为均不变；无持久化格式变化，无需重置数据。验证：`corepack pnpm run check` 通过（0 errors / 0 warnings）；全量 UI 测试通过（132 个文件、1028 项）；生产构建通过；ADR 索引覆盖 826 条记录。Prettier 检查通过新增类型文件及格式正常的 Svelte/route 文件；三个既有 TS 文件的全文件 Prettier 检查在本次修改前的 HEAD 上也失败，原因是原有无关代码排版，未把这些区域混入本切片。

## 回滚

恢复旧类型名及调用点即可；没有数据、配置或 wire 回滚步骤。
