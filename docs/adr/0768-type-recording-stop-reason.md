# ADR 0768：录音停止原因复用闭合 wire enum

## 状态

已采纳并实施（2026-10-08）。

## 背景

录音采集 pipeline 的 `RecordingReason` 只有 `Manual`、`Silence`、`MaxDuration`、`Cancel`；App 手动取消入口也只会发布 `cancel`。但 `RecordingEvent.reason`、录音 UI contract 与 overlay state 都使用开放字符串。`RecordingIndicator` 接收该 overlay reason，却只在 `isRecording` 为真时显示；开始时 reason 被清空，停止时 `isRecording` 变为 false，因而该展示分支不可达。

## 决定

1. App 定义 `RecordingStopReasonDto`，使用 `manual`、`silence`、`max_duration`、`cancel`，作为事件 wire 的唯一闭合集合 owner。它与 Input `RecordingReason` 分开：Input 拥有采集结果语义，App DTO 同时覆盖不经过 pipeline 的手动取消并拥有稳定 IPC 序列化。
2. `RecordingEvent.reason` 与停止事件 producer 使用该 DTO；录音 contract 生成并消费对应 TypeScript 类型和值清单。运行时 mapper 只接受集合内值，未知/畸形可选值沿既有策略省略；VAD signal/state 仍保留未知字符串，遵守 ADR 0340 的扩展约定。
3. 从 `RecordingOverlayState`、`RecordingIndicator` props 与模板中删除无可达展示路径的 reason。Controller 仍根据经 contract 校验的 stop reason 执行 cancel reset、Silence/MaxDuration transcription 状态转换。
4. 不改变 channel、snake_case JSON 值、事件顺序、录音/转写调度、持久数据或配置。

## 验收

- Rust 序列化测试覆盖四个闭合值和 wire spelling。
- UI contract 测试覆盖未知 stop reason 省略；overlay 状态和 toolbar 行为测试覆盖停止原因驱动的状态转换。
- 执行 `scripts/check-ipc-contracts.ps1`、`scripts/check-ipc-events.ps1`、Rust workspace fmt/test/check/clippy 与 UI check/test/build。

## 影响与回滚

事件 JSON 对合法 producer 保持不变；只在前端拒绝未知 stop reason。没有数据库、配置或用户数据迁移。回滚本切片可恢复开放字符串 mapper 与 overlay reason prop；不需要重置数据。
