# ADR 0806：类型化 Ask controller 的附件输入

## 状态

已采纳并实施（2026-10-08）。

## 背景

Ask interaction controller 负责将快捷回答和普通输入交给 chat submission callback。此前 `images` 与 `files` 在该 controller 和本地输入 payload 中都是 `unknown`，页面再将两者强转为附件数组。真实的下游 `ChatSessionController.submitMessage` 与 `submitTranscript` 已分别使用 `ChatImageAttachment[]` 和 `ChatFileAttachment[]`，上游 `InputRouter` 也按这两个形状生成附件。

## 决定

Ask controller callback、composer payload、batch submission helpers 共用已有的 `ChatImageAttachment` / `ChatFileAttachment`；删除 route 上不必要的断言。测试 fixture 使用完整的 `media_type`、`data` 与文件 `filename` 字段。

## 影响与回滚

仅收窄内部 TypeScript 接口；调用链、附件内容、转录提交与 wire payload 不变。无持久化、IPC 或配置变化，无需迁移/重置。若 attachment wire shape 变化，应在 `chatAttachmentTypes.ts` 的 owner 更新并由 submit path 验证。

## 验收

运行 UI 类型检查、完整 UI 测试和 ADR 索引检查；现有 controller 测试验证图片和文件附件沿 Ask batch 保持不变。
