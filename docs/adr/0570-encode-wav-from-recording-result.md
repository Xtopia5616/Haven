# ADR 0570：将 WAV 编码归属到录音结果

## 状态

已采纳并实施。

## 背景

`RecordingResult` 承载 InputPipeline 采集的 16 kHz 单声道 PCM。编码入口却分成两处：`InputPipeline::encode_wav(&self, pcm)` 不读取 pipeline 状态，只调用同 crate 的 helper；Tauri 录音 handler 直接调用公开的 `encode_wav_to_vec`，并重复传入采样率和声道数。Agent media tool 则使用 pipeline wrapper。相同录音结果因此经过不同入口编码。

## 决定

1. WAV 编码作为 `RecordingResult::encode_wav()` 的同步操作，由录音结果 owner 提供固定的 16 kHz 单声道格式。
2. App handler 与 media tool 都从 `RecordingResult` 编码。
3. 删除 `InputPipeline::encode_wav` 和 crate-root `encode_wav_to_vec` 导出；底层 PCM 编码 helper 留在私有 `wav` 模块。

## 替代方案

- 保留 pipeline wrapper，同时继续让 App 调用 helper：拒绝，会保留两个公开入口并继续重复格式参数。
- 将 WAV 编码迁入 App 或 Tools：拒绝，录音结果格式由 Input capture 契约决定，跨消费者复制会产生新的 owner。
- 暴露通用 WAV helper 并要求每个调用方提供采样率/声道：拒绝，当前录音结果只承诺固定的 16 kHz 单声道 PCM。

## 影响与验证

- App 与 Tool 继续得到 16 kHz、单声道、16-bit WAV；命令、文件格式和用户行为不变。
- 仅改变 workspace 内 Rust API；无数据库、配置、IPC 或事件契约变化，也无需数据重置。
- 验证：`cargo fmt --all -- --check`、workspace locked check、严格 Clippy 与串行 workspace tests。

## 回滚

恢复 helper 导出及 `InputPipeline::encode_wav`，并将两个消费者分别改回原入口。
