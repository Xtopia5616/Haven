# ADR 0803：移除未消费的录音事件字段

## 状态

已采纳并实施（2026-10-08）。

## 背景

Rust `RecordingEvent` 同时带有事件名和 `is_recording`，还包含没有 UI 读取方的 stop `duration_ms`；`TranscriptionResultEvent.confidence` 也没有展示或控制消费者。UI 按 `recording:started` / `recording:stopped` 事件名执行状态迁移，只需要 recording session id 与 stop reason；transcription 结果仍需要 session id、文本和 duration（空转写提示使用 duration）。

## 决定

从 renderer-facing `RecordingPayload` 和 `mapRecordingEvent` 输出中移除未消费的 recording `isRecording` / `durationMs` 与 transcription `confidence`。Rust wire 字段保持不变；started/stopped 事件名和 overlay controller 继续拥有状态迁移语义。

## 影响与回滚

只收窄 UI 内部事件 DTO，不改变 Rust/Tauri wire、事件顺序、空转写提示或 overlay 行为。无需数据迁移或重置。

## 验收

运行 UI 类型检查与完整 UI 测试、ADR 索引检查；契约测试验证映射不投影未消费字段，overlay 测试只提供 handler 实际读取的 payload。
