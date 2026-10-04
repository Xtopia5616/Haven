# ADR 0472：收口录音 overlay 状态与交互

## 状态

已采纳、实施并验收（2026-10-05）。

## 背景

录音 overlay 的共享 writable store 同时由 `+layout.svelte` 和 `InputRouter.svelte` 写入；layout 另行拥有时长 interval、取消命令和录音/transcription 事件状态转换。`recordingEventListeners` 已把 Rust `session_id` 映射为 `sessionId`，但状态转换忽略该身份：上一条录音的迟到 stop/error/transcription 事件可修改或清除新录音 overlay。全局录音 listener、系统通知和 voice transcript submission 同时也是 layout 的职责，不能随状态机一起搬走。

## 决定

1. 私有 `recordingOverlayController` 是 overlay store 与时长计时器的唯一写 owner；`InputRouter` 只请求 toolbar toggle，`+layout` 继续注册全局 listener 并把录音事件委托给 controller。
2. controller 按 `rec-*` 匹配 started/stopped/error/transcription 状态。终态 ID 有界记忆，阻止迟到的 started 重新显示已结束 overlay；迟到的旧 session 事件不能停掉或清理新 session 的状态和 timer。
3. 转写结果的 `submitVoiceTranscript(text, sessionId)` 始终由 layout 执行，不因 overlay session 不匹配而跳过。通知仍由 layout 决定。`AppShell` 继续只传递 presentation props，`RecordingIndicator` 继续负责展示和 Escape 回调。
4. timer 只由 controller 创建和销毁。layout 销毁时 controller 仅停止自己的 timer，不取消后台录音，也不释放 layout 持有的事件 listener；重新挂载时可恢复当前 recording timer。
5. VAD payload 没有 `session_id`，继续只在 overlay 当前为 recording 时更新；不为本切片扩展 wire contract。
6. `start_recording` 对已由 Shell/hotkey 建立的 app capture 可重发同一 ID 的 `recording:started`，用于修正 toolbar 乐观状态。controller 将同 ID 的重复 started 视为幂等确认，不重置计时。

## 不变量与验收

- `+layout` 和 `InputRouter` 不直接写 overlay store；组件只读 controller 的 state/duration。
- overlay 的 stop、error、transcription started/result/error 只影响匹配的当前 session；旧 transcript 文本仍按 payload ID 提交。
- toolbar 乐观 start/stop 失败有回滚；cancel 完成或失败都会隐藏对应 overlay，但旧 cancel completion 不清新录音。
- duration 在 start/stop/auto-stop/reset/dispose 的 timer 行为可控；dispose 不取消 pipeline，也不 dispose 全局 listener。
- 假时钟与 ID 交错测试覆盖快速启停、silence/max-duration、cancel、mute reset、旧/新转写交错、命令失败和卸载清理。

本切片不改变 Rust/TypeScript wire shape、IPC 命令签名、配置、数据库或持久数据，无需数据重置。

## 实施与验证

- 新增 `recordingOverlayController` 作为唯一 writable owner；`+layout` 保留全局事件注册、通知与 transcript submission；`InputRouter` 改为调用 toolbar toggle；删除未使用的 processing timeout。
- 迟到事件按 `rec-*` 过滤；结束 ID 使用 128 项有界集合，防止乱序的旧 started event 重新打开 overlay。重复 started 对同 ID 幂等。
- Rust start command 在 Shell/hotkey 已启动同一 App capture 时重发同 ID started event，以便 toolbar 乐观状态绑定到真实录音；事件 shape 未变。
- VAD 仍按当前 `isRecording` 门控，因为 DTO 没有 session ID；voice transcript result 仍无条件调用 `submitVoiceTranscript`，即使它只属于旧 overlay。
- 通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、`corepack pnpm run test:run`（121 个文件、973 项测试）、`corepack pnpm run build`、`pwsh -NoProfile -File scripts/check-ipc-events.ps1`。
- 未修改 wire、IPC 命令、配置或持久数据，无需重置。
