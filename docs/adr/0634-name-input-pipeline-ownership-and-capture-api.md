# ADR 0634：统一 InputPipeline owner 与采集 API 命名

## 状态

已采纳并实施。

## 背景

App runtime 对同一个 `haven_input::InputPipeline` 保存的字段叫 `pipeline`，Tools 的 platform/media dependency 则叫 `audio_pipeline`；这些引用指向同一实例和同一生命周期 owner。Input API 又混用 `start_recording`、`stop_capture`、`record_for` 与 `cancel_recording`，其中 Tauri 的用户操作词、采集设备边界和固定时长 Tool capture 没有按职责区分。`InputHandler` 实际只接收 VAD 状态和自动停止回调；`set_handler` 没有表达这是回调契约且只安装一次。

配置入口也没有标出目标：`set_limits` 接收整个 `ContextLimitsConfig`，只读取 `input_ring_buffer_secs`；`update_config` 则接受 `AudioConfig` 并更新 VAD 参数和录音时长。两种配置有不同 owner 和生命周期。

## 决定

1. 保留 `InputPipeline` 作为 `haven-input` 唯一的组合采集 owner：持有麦克风 capture engine、VAD worker、采集循环与 `RecordingState`。它不拥有转写/provider fallback、App `RecordingStatus` 或 Tools `RecordedAudio` 资产。
2. App 的 `RuntimeServices`、`ApplicationRuntime`、handler 和 Tools 的 platform/media dependencies 对该同一实例统一使用字段名 `input_pipeline`；不再用 `pipeline` 或 `audio_pipeline` 表示这一注入项。
3. Input 的常规采集入口改为 `start_capture`、`stop_capture`、`cancel_capture`；固定时长采集改为 `capture_for`。`stop_capture` 返回 `RecordingResult`，仅结束采集并交回 PCM，不执行转写。Tauri command 的 `start_recording` / `cancel_recording` 属于 App 用户操作契约，名称保持不变。
4. 回调 trait 改名为 `InputEventHandler`，安装方法改为 `install_event_handler`，明确它接收 InputPipeline 的状态/自动停止回调且只安装一次。
5. `set_limits(&ContextLimitsConfig)` 改为 `set_ring_buffer_capacity_secs(usize)`；`update_config(AudioConfig)` 改为 `update_audio_config(AudioConfig)`。内部 `config` 与 `ring_buffer_secs` 字段分别改为 `audio_config` 与 `ring_buffer_capacity_secs`。
6. `set_ring_buffer_capacity_secs` 只设定下一次 capture engine spawn 的 capacity；当前 engine 不 resize。由于 prewarm 会让 engine 持续存活，这项配置的运行时应用范围需另行决定：支持安全 resize 或标为重启生效。此项列为路线图暂缓行为，不在本 ADR 改变 capture lifecycle。
7. 保留 `RecordingState`、`RecordingResult` 和 `RecordingReason`：它们描述采集循环状态与固定格式录音结果，不与 App UI 状态 DTO 或已登记媒体资产结果合并。

## 替代方案

- 将 `InputPipeline` 改成 `AudioRuntime`：拒绝，`haven-tools` 已有独立 `AudioRuntime`，它负责 media tool 的设备/播放/资产适配，而 InputPipeline 拥有底层采集与 VAD 生命周期；改名会制造跨 crate 同名异义。
- 将 Input 采集结果、App `RecordingStatus` 与 Tools `RecordedAudio` 合并：拒绝。三者分别表示 PCM 采集结果、shell/UI 快照和已登记 WAV 资产，owner、字段约束与生命周期不同。
- 让所有低层操作继续用 `recording`：拒绝。Input API 只负责采集，命名为 capture 可以明确它不负责 transcript/provider 阶段；App IPC 仍保留用户操作术语。

## 影响与验证

- 修改仅涉及 Rust 内部跨 crate API/字段和架构/命名文档；Tauri command/event、配置序列化、数据库 schema 与录音行为不变。
- 更新 Input、App 与 Tools 的所有生产调用点和测试实现；检索确认旧 API/注入字段不再存在于当前源码。
- 运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked` 和 `cargo clippy --workspace --locked -- -D warnings`；应用测试时使用 `target` 下新的隔离 APPDATA。

## 回滚

将 Input API、trait、配置入口和跨 crate 注入字段恢复为旧名称并同步还原调用点与文档。没有配置、数据库或 IPC 重置要求。
