# ADR 0847：区分录音身份与会话身份

## 状态

Accepted — 2026-10-10

## 背景

Haven 的 `session` 表示用户与 Agent 的对话，持久会话 ID 使用 `ses-*`。交互录音使用进程内 `rec-*`，但此前 Rust 把录音 ID 表示为 `SessionId`，录音生命周期 owner、事件 DTO、`process_transcript` 参数和 UI overlay 也使用 `session_id` / `sessionId`。同一术语因此同时指向对话与一次麦克风采集，增加了将录音关联误用为会话关联的风险。

录音 ID 只负责将一次采集、停止、转写和语音提交关联起来；`InputPipeline` 还服务于定时工具采集，不能把所有采集身份都归为 App 录音。录音身份由 App 控制生命周期的 owner 持有，不写入数据库。

## 决定

- 将 Common 的运行时 ID newtype `SessionId` 改为 `RecordingId`，继续使用 `rec-*` 前缀；`session_id` 与 `sessionId` 只表示对话。
- 将 App owner、状态字段和 guard 命名为 `RecordingLifecycleOwner`、`recording_lifecycle`、`current_recording_id` 与 `RecordingLifecycleGuard`。
- 将录音与转写事件 DTO 的 wire 字段统一为 `recording_id`；前端 mapper 只接受该字段，并输出 `recordingId`。录音浮层状态、controller、语音提交参数及 `process_transcript` 的 Tauri 参数也统一使用 `recordingId`，Rust 命令参数使用 `recording_id`。
- 删除旧命名，不保留别名或旧字段读取路径。Bundled UI 与 Rust 命令/事件同步更新；独立旧 renderer 的旧 wire 字段不受支持。
- 不改变采集、停止、取消、转写调度及事件顺序，不改变 ID 前缀，也不修改持久化 schema。

## 替代方案

- 保留 `SessionId` 并仅通过注释区分：拒绝。类型和事件字段仍会把不同领域身份混为一谈。
- 统一使用 `String`：拒绝。App Rust 层有实际的类型隔离需求，`RecordingId` 使生命周期交接处可由编译器区分录音与其它 ID。
- 将定时工具采集也纳入 App recording owner：拒绝。工具采集共享输入管线，但不受 UI 录音控制，也不产生录音浮层事件。

## 影响与验证

这是运行时 IPC/event 命名变更。`rec-*` ID 不落库，pending usage 也只保存在进程内；无需数据库迁移或重置。旧 `session_id` 录音事件不再被 UI 当作录音身份，`process_transcript` 请求字段变为 `recordingId`。转写文本仍只进入当前会话提交流程，录音 ID 不成为 session identity。

验证通过：`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 和 Rust 格式检查；UI Svelte 检查（0 errors / 0 warnings）、全量测试（131 个文件、1029 项）与生产构建；IPC 命令生成契约（80 handlers）和事件目录（36 channels）；ADR 索引（830 条记录）；本切片修改的 UI 源码、路由与 ADR Prettier 检查。录音 event/command 的 UI 映射测试验证新字段，旧 `session_id` 不作为录音身份读取。

## 回滚

无持久化回滚步骤。若需回退，应将 Rust DTO/命令参数、生成契约、UI mapper 与所有消费者作为一个整体恢复；不得只为旧 renderer 增加双字段兼容逻辑。
