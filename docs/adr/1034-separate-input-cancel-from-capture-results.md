# 1034：将取消保留在 App 录音生命周期

## 状态

已采纳并实施（2026-10-12）。

## 背景

Input `RecordingResult` 只由正常停止的采集 loop 产生，真实原因是 `Manual`、`Silence` 或 `MaxDuration`。`cancel_capture()` 丢弃缓冲数据并返回 `Result<()>`，不会产生 `RecordingResult`。但 `RecordingReason::Cancel` 和 App 对应分支只由合成测试 fixture 使用；实际 `cancel_recording` command 直接发布 App-owned `RecordingStopReasonDto::Cancel`。

## 决定

- 从 Input `RecordingReason` 删除没有生产来源的 `Cancel`，只表达成功交付 `RecordingResult` 的采集停止原因。
- 删除 App 对 Input 结果 `Cancel` 的映射和不可达的“不调度转写”结果分支。
- App 手动取消继续调用 `cancel_capture()` 并直接发布 `RecordingStopReasonDto::Cancel`；UI wire 值和命令行为不变。

## 影响

Input crate 的公开 Serde enum 收窄为三种真实结果原因，不影响 Tauri、持久化或配置契约。该结论更新了 ADR 0634/0768 中将 Input enum 列为四种原因的历史描述；那些 ADR 保留作为历史记录。

## 验证

- `cargo test --locked -p haven-input`
- `cargo test --locked -p haven-app-binary commands::recording::tests`
- `rustfmt --check` 与全仓检索 `RecordingReason::Cancel`

## 回滚

若未来 Input pipeline 需要把取消作为可返回结果，必须新增实际返回该结果的生产路径，并证明录音数据不会被误交付转写；不能只恢复一个无 producer 的 enum 变体。
