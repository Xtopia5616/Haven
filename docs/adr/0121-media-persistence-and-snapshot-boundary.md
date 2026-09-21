# ADR 0121：多模态表示持久化与快照边界

日期：2026-09-10
状态：已采纳

关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)、
[ADR 0115：受管上传资产生命周期与清理](0115-managed-upload-lifecycle-and-cleanup.md)、
[ADR 0116：多模态请求与受管上传边界加固](0116-media-boundary-hardening.md)

## 背景

前一阶段已经有 `MediaAsset`、`MediaRepresentation` 和 `MediaPlan`，但运行时仍
可能把同一份图片/音频 bytes 同时写入 `MessageAttachment`、消息 UI 元数据
和 ReAct snapshot；OCR/STT 成功时也只把派生文本拼进正文，原始附件会被请求链路
丢掉。这会造成快照膨胀、resume 语义分叉，以及无法按表示重新规划 provider 请求。

## 决定

1. `messages.media_inputs` 是消息的 provider-neutral 媒体持久化投影，包含资产元数据、
   表示、provenance、availability 和 preferred representation。`messages.ui_metadata`
   只保留 UI/资产保留元数据，不保存 base64 或 provider 表示；受管路径只在可信 host
   根目录内为历史预览重新读取 bytes。
2. `TranscriptRecord::UserInject` 新写入 `media_inputs`，消息 `attachments` 只作为
   ingress/UI DTO；旧数据库不迁移，按 schema reset 边界重建 `ui_metadata`。
   旧快照加载时立即转换并清空旧字段；之后的 snapshot 不会再序列化 inline bytes。
   `CompactSummary` 也在序列化边界把 inline image/audio 替换为带 MIME 的安全标记。
3. OCR/STT 成功只在 `media_inputs` 上追加持久化的派生 `MediaRepresentation`，设置
   preferred kind，并保留原始表示以供 retry、resume 和其他 provider 重新规划。
   正文中的 provenance fence 只作为当前 UI/旧消息的展示投影。
4. app-binary 对图片、音频和普通文件统一生成受管文件；gateway 在进程内仍可读取
   短暂的 base64，数据库和 snapshot 边界负责剥离它。普通文件继续通过 `files(asset_id)`
   访问；本阶段不新增视频 `ContentPart` 或独立媒体服务。
5. 数据库 schema 从 v16 提升到 v17；后续 v24 再将旧 `attachments` 列收敛为
   `ui_metadata`。项目当前不做运行时迁移，旧数据库按发布说明重置。

## 失败安全与恢复

- host-owned 根目录之外的路径永远不会用于历史预览重读。
- 快照恢复优先使用可读的受管附件；只有没有可恢复 bytes 时才使用
  `media_inputs` 的 metadata-only 投影，不伪造 inline 数据。
- 旧 snapshot、旧 message JSON 和缺少 `media_inputs` 的消息仍可读取；重新保存后会落到
  新的 metadata-only 形状。

## 验证

覆盖 common media plan、gateway 派生表示、消息 schema/读写、旧 snapshot 迁移、
compact summary 脱敏、managed asset resume，以及 app-binary 图片/音频统一落盘。
Rust 与 UI 门禁按仓库开发标准执行。
