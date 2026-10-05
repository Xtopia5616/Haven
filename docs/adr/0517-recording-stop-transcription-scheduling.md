# ADR 0517：统一录音停止后的转写调度

## 状态

已采纳，已完成（2026-10-06）。

## 背景与证据

同一 App-owned capture 有两个成功停止入口：`stop_recording` Tauri command 与 `HavenShellHandler::on_recording_stop`。command 会停止 capture、分离 `rec-*`、同步 shell/tray、发布 `recording:stopped`，然后通过 `ApplicationRuntime::spawn` detached transcription。Shell handler 也会停止 capture、分离 ID 并发布 stopped，但随后直接 await `finalize_transcription`；外层 `DesktopShell` 要等 handler 返回后才派发 tray 更新。

因此慢 STT 会让 hotkey/VAD/mute/hold stop 的 tray 与调用返回延迟，而 toolbar stop 已先结束 UI recording 状态。现有测试未用受控 STT gate 固定两条路径的完成顺序。此次只收敛成功 stop 后的调度，不调整 capture、转写或 transcript submission 策略。

## 决定

1. `commands::recording` 提供共同的成功 stop 完成与调度入口。两个 adapter 在 `stop_capture` 成功、持有 `RecordingLifecyclePermit` 时分离当前 ID，再调用该入口；Shell handler 不再直接 await `finalize_transcription`。
2. 成功路径顺序固定为：capture 已停止 → 旧 `rec-*` 已 detach → 按入口语义刷新当前 tray → 发布 `recording:stopped` → 对 `Silence`/`MaxDuration` 执行代次匹配的 toggle reset → 释放 lifecycle permit → 注册 transcription task。这样 finalizer 即使立即开始，停止状态和终态事件也已先发布，且旧转写期间可建立下一条录音。
3. Shell stop handler 不回写 `is_recording=false`：Shell 入口已先写状态，handler 只从当前状态刷新 tray，避免迟到的旧停止覆盖 handler 等待期间到来的新 toggle/start。ShellState 另维护 recording revision 与 toggle generation；Tauri command 与 Shell start adapter 的异步状态同步只在录音 revision 未变化时应用，自动停止 reset 只在 toggle generation 与 stop 开始时捕获的代次匹配时应用。代次不匹配时保留较新的输入意图。
4. 自动停止 reset 在释放 lifecycle permit 前进行。`Manual` 不触发该 reset；`Cancel` 不走转写调度。
5. 唯一转写调度 owner 使用 `ApplicationRuntime::spawn("recording-transcription", ...)`。不得用裸 `tokio::spawn`；runtime 关闭后拒绝注册时记录 warning，不留下未受管任务。已有 command 路径对 shutdown 的语义保持不变。
6. 保留停止失败的既有分类与 `recording:error` ID 关联。没有 App-owned recording ID 的 timed `media.record` 路径只刷新 Shell tray 并返回，不停止 pipeline、不发布 voice recording/transcription 事件。
7. 不改 `recording:*`、`transcription:*` IPC payload、transcription provider 策略、UI overlay owner、持久数据、配置或 crate 依赖方向。

## 替代方案

- Shell 继续直接 await STT：保留两套调度语义和 tray 延迟，拒绝。
- 只让 Shell 尽早返回、仍各自安排 task：调度标签、shutdown 行为和先后顺序继续分叉，拒绝。
- 裸 `tokio::spawn`：绕过 AppRuntime 的 shutdown cancellation/join owner，拒绝。
- 仅依赖 lifecycle permit 保护 Shell toggle：Shell 在调用 handler 前就更新状态，permit 无法阻止迟到的 stop/reset 覆盖较新的 toggle，拒绝。

## 验收

- 共享 stop 完成入口的确定性测试用 gate 阻塞 finalizer，证明 tray dispatch 与 stopped 发布先发生、stop adapter 不等待 finalizer、permit 已释放后可开始新录音，且延迟完成仍携带旧 `rec-*`。
- 覆盖 auto-stop reset 的代次匹配/不匹配、`RefreshCurrent` 保留较新 toggle、Tauri recording revision 匹配/不匹配、manual stop 不 reset、Cancel 不调度。
- 用注入的 stop closure 验证无 App owner 时不调用 `stop_capture` 且 capture 仍为 `Recording`；用失败仲裁 helper 覆盖 Pending / Processing 成功返回、同一 `rec-*` error 关联、Recording 原错误返回且不做副作用。
- 生产调用链复核确认两个成功入口都经共同完成 helper 与唯一 `ApplicationRuntime::spawn("recording-transcription", ...)` scheduler；Cancel 直接完成 detach/stopped 发布、不调用 scheduler，共同 helper 也防御性跳过 `Cancel` reason，且有回归测试；生产源码无裸 `tokio::spawn`。gate 测试证明共同 helper 不等待受控转写任务。Tauri command 和 runtime shutdown 不额外构造 headless AppHandle 集成夹具。
- 适用门禁：`cargo fmt --all -- --check`、`cargo test --locked -p haven-app-binary`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-crate-dependencies.ps1`、ADR index 与 `git diff --check`。不涉及 IPC shape 或 UI，因此不运行前端 IPC/UI 门禁。

## 影响与回滚

该切片只改变 app-owned recording adapter 的 stop 后处理时序和内部调度入口，不修改持久化、用户数据、配置或 wire contract，无需重置。若回滚，恢复 Shell handler 直接等待 `finalize_transcription` 即可；这会重新引入慢 STT 阻塞 Shell tray 更新的问题。

`commands/recording.rs` 当前共 1,189 行，其中生产逻辑 722 行、同文件测试 467 行，超过开发规范的 800 行职责复核阈值；本切片保留 helper 与现有 recording command owner 同文件，因为它直接协调同一 `rec-*`/event/finalizer 生命周期，当前没有第二个独立消费者或更稳定的拆分接口，附件 ingress 已在私有子模块。拆文件不会进一步收敛所有权；若后续出现独立 recording 子域或测试职责漂移，再以证据单独立项。

## 实施记录

- 两条成功停止路径共用完成/调度入口；Shell 使用 latest-state tray refresh，Tauri 以 recording revision 作条件同步，自动 toggle reset 以独立 generation 作条件更新。
- 新增无 owner stop gate、stop failure 仲裁、error event ID 过滤、`RefreshCurrent` 新 toggle 与 Cancel 不调度的回归测试；最新 `haven-app-binary` 测试 228 passed。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 均通过。Workspace 测试输出含已有 Windows linker `.lib/.exp` 提示；未见测试失败。依赖清单、ADR index 与最终 diff 检查待提交前复核。
- 没有 IPC 或持久数据变更，无需数据重置；Windows 发布验收仍按 roadmap §5.1 独立开放。
