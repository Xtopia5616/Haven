# ADR 0122：工具媒体请求统一入口

日期：2026-09-10
状态：已采纳

补充：媒体编排归属与 gateway 删除由 ADR 0130 取代；本 ADR 仅保留
`LlmRouter::analyze_image` 作为 provider-facing vision 原语的历史决策。

说明：本 ADR 的请求入口决定仍然有效；模型工具契约与跨工具资产引用由后续 ADR 0123
进一步收敛。

> 当前公共工具入口由 ADR 0133 进一步统一为 `media`；这里的历史 `audio.record` 指
> `media(operation="record")`。

关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)、
[ADR 0121：多模态表示持久化与快照边界](0121-media-persistence-and-snapshot-boundary.md)

## 背景

媒体资产已经有 `MediaInput → MediaPlan → ContentPart` 的规划链路；历史上的
`MediaGateway` 已由 ADR 0130 删除，当前 `files`、`window` 和 `media` 的录音分支只负责
producer/设备边界，媒体理解统一由共享的 `MediaTool` 承接。这样能力校验、
提示词边界、fallback 和 provider 适配不会在入口之间漂移。

## 决定

1. `haven_llm::LlmRouter::analyze_image` 只保留为 provider-wire adapter：它接收
   已读 bytes 并序列化一次 vision 请求，负责 router 的 provider 重试/限流/适配器
   校验；它不负责 asset lookup、工具权限、生命周期、跨 provider fallback 或
   模型可见的媒体编排。
2. `files`、`window.ocr` 和 `media(operation="record")` 共享一个 `Arc<MediaTool>`；它们只负责
   路径/设备边界、大小限制、取消和 producer 结果。OCR、STT、confidence、timeout、
   fallback 以及失败时的完整 `media` 引用均由 `MediaTool` 统一生成。
3. `files.read` 对按扩展名识别的音频交给同一 `media.transcribe` 策略，不再给出
   不可执行的“audio 工具转写”建议。
4. 删除只接受 raw bytes 的 `image_part_from_bytes` / `audio_part_from_bytes` 兼容
   辅助函数。raw bytes 必须经过上层媒体入口；保留只处理已规划 base64 的内部
   projection constructors。

## 替代方案

- 继续在每个工具中维护一份消息拼装代码：实现最小，但会继续产生 provider 能力
  和安全策略漂移，拒绝。
- 让工具重新持有独立的媒体 gateway：会复制 provider 编排、破坏统一权限/取消
  契约，拒绝。
- 新增独立 `media` 工具并删除现有 `files`/`window` 能力：能统一入口，但会扩大
  模型工具契约和 UI renderer 的变更面；本阶段先复用现有操作，后续若需要跨工具
  资产引用再单独立项。

## 影响与重置

这是 Rust crate 内部的破坏性 API 收缩：删除的两个 raw-byte helper 不再可调用；
当前 workspace 无生产调用方。不会改变数据库 schema、snapshot 或已有工具名称，
无需数据重置。已有 `MessageAttachment` 兼容投影本阶段不删除，避免把 UI/历史
展示迁移与 provider 请求边界混在同一提交中。

## 文件边界

`files.rs`、`window.rs` 和 `audio.rs` 仍是各自的 producer/设备边界，但不再持有
provider 编排。媒体编排、媒体结果契约与受管资产导航集中在
`haven-tools/src/builtin/media.rs`；`haven-llm/src/media/vision.rs` 只保留一次
provider-wire 请求适配。视频 native wire 等后续能力另行评审。

## 验证与回滚

- `cargo test --locked -p haven-llm media::vision -- --nocapture`
- `cargo test --locked -p haven-tools builtin::files -- --nocapture`
- `cargo check --locked -p haven-llm -p haven-tools`
- 回滚本提交即可恢复旧的工具内消息拼装；无持久化回滚动作。
