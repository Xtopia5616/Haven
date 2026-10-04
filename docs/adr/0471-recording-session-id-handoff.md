# ADR 0471：分离录音 session ID 与异步转写生命周期

## 状态

已采纳、实施并验收（2026-10-05）。

## 背景

`InputPipeline::stop_capture` 在把本次 `RecordingResult` 交回调用方前会将采集状态恢复为 `Pending`，因此下一次采集可以在上一条 STT 完成前开始。App 过去把当前 `rec-*` 放在一个可选共享槽位中，`finalize_transcription` 直到 detached task 首次运行时才 `take()`。Tauri 命令 stop 还会先同步 Shell、发 stop 事件并提交后台任务；Shell/VAD 路径在 finalizer 前也可能 await。新 start 因而可能复用旧 ID，之后旧转写终态就无法可靠地区分新旧 overlay。

按钮命令、快捷键、VAD 自动停止、静音停止和取消都操作同一个 `InputPipeline`，但 Shell 内部状态锁不会覆盖异步 handler。另有 timed `media.record` 直接使用同一 pipeline；它不产生 UI recording session，不能被 App voice 命令误认或停止。

## 决定

1. `AppState` 使用 `RecordingSessionOwner` 作为 App voice 命令与 Shell handler 共同的身份 owner。它提供 async lifecycle permit 和当前 `rec-*`；`begin/current/finish` 必须持有该 permit。
2. App command 与 Shell handler 的 start/stop/cancel 在调用共享 pipeline、更新 Shell 状态和发布对应身份事件期间持有同一 permit，消除 App 入口之间的交错。stop 成功后在 permit 释放前分离本次 ID，并将 ID 显式传给转写 finalizer；finalizer 不再读取或清空“当前录音”槽位，STT 工作在 permit 外运行。
3. stop 和 cancel 事件通过现有可选 `RecordingEvent.session_id` 携带被结束录音的 `rec-*`。stop 失败若能确定活动 capture，也沿用该 ID；尚未建立 capture 的 start 失败仍生成独立 ID。
4. timed `media.record` 不持有 App `RecordingSessionOwner`，不生成 voice `rec-*`。当 App start 遇到 pipeline 正在录音但没有 App-owned ID 时，报告设备被其他操作占用；App stop/cancel 只控制带有 App-owned ID 的录音，不接管工具 capture。`InputPipeline` 仍负责底层设备状态互斥。
5. 不修改数据库、配置、Tauri 命令签名或 durable event；`recording:stopped` 只是开始填充其已有的可选 `session_id` 字段。

## 不变量与验收

- 每次成功的 App voice capture 都有独立 `rec-*`；command、Shell/hotkey、stop 和后续 transcription 共用该 ID。
- stop/cancel 清理当前身份必须先于 permit 释放，因此下一次 App start 不可能复用刚结束的 ID。
- finalizer 使用 stop 路径捕获的 ID；旧 STT 可在新 capture 期间继续，但不能读写新 capture 的当前 ID。
- 同时到达的 App stop 路径最多有一个分离当前 ID；后续 stop 看到无 App-owned ID 后不重复停止 pipeline。
- timed `media.record` 没有 App-owned ID 时，voice stop/cancel 不执行 pipeline stop/cancel；共享 pipeline 的状态检查继续阻止两种 capture 同时启动。
- 使用可控并发测试验证“旧 ID 分离后新 start 获得新 ID”与“并发 stop 只有一个成功 detach”。完整门禁还覆盖 Rust workspace、UI mapper/状态消费和 IPC channel 漂移。

本切片不改 UI overlay 所有权。后续单独评估 `recordingOverlay` store 的双写入口及 `+layout.svelte` timer/state/cancel；全局 Tauri 事件登记、系统通知和 voice transcript submission 仍由 layout 持有。UI controller 必须按 `rec-*` 忽略旧 transcription 对新 overlay 的状态修改，但仍提交旧结果文本。

## 实施与验证

- `AppState` 以 `RecordingSessionOwner` 和生命周期 permit 串行化 App voice command 与 Shell handler 的 start/stop/cancel；timed `media.record` 不创建或消费 App voice ID。
- stop/cancel 在允许下一次 start 前分离当前 ID；command 与 Shell finalizer 都显式接收本次 ID。
- 增加 owner handoff 与并发 stop 单次 detach 回归测试。
- 通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、`pwsh -NoProfile -File scripts/check-ipc-events.ps1`。
- 无 schema、配置或 durable event 变更，无需重置数据。

## 替代方案

- 只在 `finalize_transcription` 开始时继续 `take()`：后台 task 调度存在延迟，无法保证 next start 前身份已被分离，拒绝。
- 单独增加 AppState ID mutex 而不串行 command/Shell 路径：每条路径可分别停止或更换当前 ID，仍会出现 stop/start 交错，拒绝。
- 让 UI 按事件到达时间猜测录音归属：跨越 detached STT 的交错不是可靠身份规则，拒绝。
- 让工具录音共用 App voice session ID：timed `media.record` 不属于 UI voice ingress，混用会造成虚假 started/transcription 事件，拒绝。

## 影响与回滚

只影响 App 内的 voice lifecycle coordinator、录音 command/Shell adapter、现有 stop event 的可选 ID 值和对应文档/测试。无数据库、配置或用户数据重置要求。回滚本 ADR 的实现会重新引入 ID 复用窗口，并恢复 voice stop/cancel 对无 owner capture 的接管风险。
