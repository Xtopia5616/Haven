# ADR 0165：采集与媒体派生、provider fallback 边界

日期：2026-09-15
状态：已采纳

关联：[ADR 0130：媒体编排统一下沉到工具层](0130-media-orchestration-in-tool-layer.md)、
[ADR 0133：统一媒体与音频的模型工具契约](0133-unified-media-audio-tool-contract.md)、
[ADR 0136：多模态探测、表示与结果契约统一](0136-canonical-multimodal-contract.md)

## 背景

`InputPipeline` 同时持有录音状态、专用 STT client 和 LLM router，导致用户语音入口与
`MediaTool` 各自实现一份 provider 选择、超时、空文本和 fallback 语义；`record` 的能力
快照还曾把“能否采集”错误地绑定为“是否已配置 STT”。这会使无 STT 时无法保留录音资产，
也会让 capability unavailable 与 provider 执行失败混在同一条错误路径中。

## 决定

1. `haven-input::InputPipeline` 只负责麦克风采集、VAD、录音生命周期、PCM/WAV 序列化和
   采集侧错误。它不持有 `SttClient`、`LlmRouter`，也不执行 provider 调用。
2. `haven-tools::builtin::media` 提供唯一的媒体转写策略：专用 STT 优先；结果为空、低于
   置信度或 provider 失败时，最多回退到 `LlmRouter::transcribe_audio`。应用语音入口通过
   `ToolsManager::transcribe_recording` 使用同一策略，不能直接选择 provider。
3. `record` 的 capability 只由采集管线是否接入决定；`transcribe` 由专用 STT 或可用的
   LLM 音频路由决定。能力快照由同一个 `MediaCapabilities` 同时驱动 schema、媒体引用和
   runtime prompt。
4. 未配置 capability 是成功的结构化结果，包含 `available: false`、`capability`、稳定的
   `reason_code`（如 `transcribe_unavailable`）；provider 调用失败、超时和取消仍保留各自
   的执行结果语义，不伪装成 capability unavailable。
5. 录音的采集错误在 `RecordingResult::capture_error` 中返回；它不会进入 provider fallback，
   也不会伪造成转写结果。

## 替代方案

- 继续在 InputPipeline 保留 STT 入口：拒绝，会重复实现 MediaTool 的 fallback 和 capability
  规则，并保留 input→llm 的反向依赖。
- 无 STT 时禁用 `record`：拒绝，录音资产本身可被保留并在 provider 恢复后派生。
- 用错误字符串推断能力：拒绝，结构化 unavailable 结果必须与 provider 执行失败可区分。

## 影响与验证

- `haven-input` 删除 `haven-llm` 运行时依赖；`build_stt_client` 不再接收无意义的 router。
- UI 语音事件形状不变；应用只把 capture error 或媒体转写结果映射到既有事件。
- 无数据库 schema、快照或配置迁移；这是运行时 crate/API 边界的破坏性清理。
- 验证：input、tools、app-binary 定向 check/test，以及 workspace fmt/check/test/clippy；重点
  覆盖无 STT 仍可 advertise `record`、无 provider 的稳定 capability result、空转写和采集静音。

## 回滚

回退本 ADR 对应提交即可，无需数据库重置。若运行态快照跨版本混用，按既有测试版策略清理
运行态快照和媒体缓存，不增加兼容双路径。
