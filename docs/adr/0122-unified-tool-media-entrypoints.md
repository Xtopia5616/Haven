# ADR 0122：工具媒体请求统一入口

日期：2026-09-10
状态：已采纳

说明：本 ADR 的请求入口决定仍然有效；模型工具契约与跨工具资产引用由后续 ADR 0123
进一步收敛。

关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)、
[ADR 0121：多模态表示持久化与快照边界](0121-media-persistence-and-snapshot-boundary.md)

## 背景

媒体资产已经有 `MediaInput → MediaPlan → ContentPart` 的规划链路，但
`files`、`window.ocr` 和 `MediaGateway` 仍各自读取 bytes、编码 base64、拼装
图片消息。这会让能力校验、提示词边界和 provider 适配逐渐漂移；同时 `files`
对音频只返回“使用 audio 工具转写”的提示，而 `audio` 工具并没有文件转写入口。

## 决定

1. `haven_llm::LlmRouter::analyze_image` 是工具和 gateway 做一次性图片理解的唯一
   高层入口。它负责 vision role 选择、`CapabilityProfile` 规划、base64 编码、
   canonical image part 构造以及正常的 router 重试/限流/适配器校验。
2. `files` 和 `window.ocr` 只负责路径边界、大小上限、取消和工具结果；不再直接
   构造 `ContentPart::Image`。gateway 的 OCR fallback 也使用同一入口。
3. `files.read` 对按扩展名识别的音频调用已有的 `LlmRouter::transcribe_audio`，
   在没有 router、超限、失败或超时场景返回结构化结果，不再给出不可执行的
   “audio 工具转写”建议。
4. 删除只接受 raw bytes 的 `image_part_from_bytes` / `audio_part_from_bytes` 兼容
   辅助函数。raw bytes 必须经过上层媒体入口；保留只处理已规划 base64 的内部
   projection constructors。

## 替代方案

- 继续在每个工具中维护一份消息拼装代码：实现最小，但会继续产生 provider 能力
  和安全策略漂移，拒绝。
- 让 `haven-tools` 直接持有 `MediaGateway`：会把 gateway 的专用客户端生命周期
  和 provider 编排泄漏到工具装配，破坏依赖方向，拒绝。
- 新增独立 `media` 工具并删除现有 `files`/`window` 能力：能统一入口，但会扩大
  模型工具契约和 UI renderer 的变更面；本阶段先复用现有操作，后续若需要跨工具
  资产引用再单独立项。

## 影响与重置

这是 Rust crate 内部的破坏性 API 收缩：删除的两个 raw-byte helper 不再可调用；
当前 workspace 无生产调用方。不会改变数据库 schema、snapshot 或已有工具名称，
无需数据重置。已有 `MessageAttachment` 兼容投影本阶段不删除，避免把 UI/历史
展示迁移与 provider 请求边界混在同一提交中。

## 文件边界

`files.rs` 和 `window.rs` 仍是较大的工具实现，但本变更没有继续把 provider
协议逻辑堆回其中：文件系统读写、桌面捕获、大小限制和工具结果仍属于各自工具；
媒体编码与能力规划已移到 `haven-llm/src/media/vision.rs`。后续若新增视频/表格
等媒体操作，应在 `haven-llm::media` 增加对应高层入口，再由工具调用，而不是继续
扩张这两个工具文件。

## 验证与回滚

- `cargo test --locked -p haven-llm media::vision -- --nocapture`
- `cargo test --locked -p haven-tools builtin::files -- --nocapture`
- `cargo check --locked -p haven-llm -p haven-tools`
- 回滚本提交即可恢复旧的工具内消息拼装；无持久化回滚动作。
