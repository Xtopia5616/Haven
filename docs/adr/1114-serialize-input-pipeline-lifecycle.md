# ADR 1114：串行化 InputPipeline 生命周期并 join 录音 worker

## 状态

已接受（2026-10-13）

## 背景

`InputPipeline::start_inner` 只短暂检查 `RecordingState`，随后会 await engine/VAD 初始化，再安装 cancellation/result 通道。`shutdown` 可以在句柄安装前返回，原 start 仍会在 shutdown 后启动录音；shutdown 之后也没有禁止 `start_capture` 或 `prewarm` 重建资源。VAD worker 和 audio capture engine 的 OS 线程句柄此前都被丢弃，资源释放不代表线程已退出。

Normal recording 的 auto-stop 回调也没有录音身份。它以 detached task 调用 shell，并在 callback 实际执行时停止“当前”录音，迟到请求可能影响下一次录音。回调仍必须异步于 recording loop：它会等待该 loop 的结果，若 loop 内直接 await callback 会自等待死锁。

## 决定

- `InputPipeline` 用一个 lifecycle gate 串行化 prewarm、start、stop、cancel 和 shutdown。shutdown 首次进入时设置终态 closed；后续 prewarm 无操作、capture start 明确失败、重复 shutdown 幂等；关闭状态不允许重新创建 engine 或 VAD worker。
- audio engine 与 VAD worker 各自保留 OS `JoinHandle`。shutdown 先取消并 join recording loop，再向 capture engine 发关闭命令、关闭 VAD sender，并在 blocking pool join 两个 OS worker；并发 shutdown 调用等待同一 drain。
- Pipeline owner 用 `JoinSet` 持有 auto-stop callback，启动下一项 callback 前回收已完成句柄，shutdown join 剩余 callback，最多等待 5 秒后 abort 并收割。
- App-owned Normal capture 在打开麦克风前登记唯一 `rec-*` identity，Input 将该 identity 随 loop 传入 auto-stop callback。App 在 `RecordingLifecycleOwner` guard 下校验 identity；匹配后只领取当前 active shell stop 并直接走共同 finalization helper。迟到 identity 不修改 shell、capture 或当前 recording ID。
- Auto-stop callback 继续在 recording loop 外执行。Timed `media.record` 不分配 app recording ID，也不触发 user recording callback。
- 不修改数据库、durable event、Tauri wire payload 或消息内容；录音身份仅用于既有 app 内 lifecycle 关联。

## 替代方案

- 在 recording loop 里直接 await auto-stop handler 会形成 handler 等待 loop、loop 等待 handler 的自等待。
- callback 继续调用无 identity 的 `DesktopShell::stop_recording`，只能在迟到时停止届时的当前录音，不能证明请求属于哪次 capture。
- 只 drop worker sender 会最终让 OS thread 退出，但无法确认模型初始化、推理或 WASAPI teardown 已完成。

## 影响

Pipeline 生命周期关闭后不可复用；应用只在最终 shutdown 调用该 API。Normal recording 的 `rec-*` 在启动尝试开始时登记，启动失败会条件清除尚未建立的 identity。Auto-stop task 与两个 native worker 都有明确 owner；worker join 走 blocking pool，避免阻塞 async executor。callback 超过 5 秒会被 abort 并记录告警。

不要求数据或配置重置：本切片只调整进程内录音生命周期，不改 schema、持久字段或外部 wire contract。

## 验证

- `cargo test --locked -p haven-input`
- `cargo test --locked -p haven-app-binary`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- IPC contract、ADR index 与 `git diff --check`

测试覆盖关闭终态和幂等、start/shutdown gate、prewarm resource join、capture/VAD OS thread join、auto-stop callback join 与录音 identity 传递、stale identity 不能匹配/清除新录音。

## 回滚

不得恢复无 owner 的 worker thread 或无录音 identity 的 auto-stop。若取消终态 closed 约束，必须先定义 shutdown 与重启的 owner/资源重建边界，并覆盖 start、prewarm 与 shutdown 的并发交错。
