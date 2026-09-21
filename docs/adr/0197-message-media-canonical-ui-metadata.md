# ADR 0197：消息媒体 canonical 与 UI 元数据列边界

日期：2026-09-21
状态：已采纳

## 背景

`messages.media_inputs` 已经是消息媒体的 provider-neutral canonical 持久化投影，
但消息表仍使用名为 `attachments` 的兼容列。这个名称容易让读写方把 UI/ingress
对象误认为第二个媒体事实来源；仓储还会把 provider 表示和偏好复制进该列，造成
双重持久化与恢复语义分叉。

## 决定

1. `messages.media_inputs` 是媒体资产、表示、provenance、availability 和 preferred
   representation 的唯一持久化事实来源。
2. `messages.attachments` 保留为内存中的 ingress/UI DTO，不再作为数据库列名。
   数据库使用 `messages.ui_metadata`，只保存 UI 展示与受管资产保留所需的
   `asset_id`、MIME、文件名、受管路径、hash、大小和 expiry；不保存 base64、派生
   `MediaRepresentation` 或 provider 偏好。
3. 仓储读取 `ui_metadata` 后重建 `Message.attachments`，仅从受信 host 根目录重新
   读取历史预览 bytes；provider 规划、事件恢复和媒体派生只读取 `media_inputs`。
4. schema 从 v23 提升到 v24。没有运行时迁移；已有数据库按发布说明删除并重建。

## 影响与验证

这会保留历史消息 UI 所需的文件名、路径和预览重建能力，同时让列名、JSON 形状和
代码注释明确区分 UI metadata 与 canonical media。验证包括 schema 列契约测试、
消息仓储 roundtrip、受管预览重建和完整 `haven-memory` 测试。
