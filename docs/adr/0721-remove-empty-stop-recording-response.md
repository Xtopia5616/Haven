# ADR 0721：移除 stop_recording 的空字符串成功响应

## 状态

已采纳并实施。

## 背景

`stop_recording` 在所有成功路径都返回 `String::new()`：正常停止后启动受管的后台转写再返回空串；如果另一条路径已开始 finalization，也返回空串。用户可见的 stopped/result/error 状态由 `recording:*` 和 `transcription:*` 事件发布。`RecordingOverlayController` 只 `await` 命令，不读取其成功值；IPC 生成契约却仍把响应暴露为 `string`，保留了旧的文本结果外形但没有实际内容。

## 决定

- 将 Tauri handler 改为 `Result<(), String>`，Rust unit 成功值经 generated IPC 映射为 `void`；停止错误仍通过命令错误返回。
- `recording:stopped` 与 `transcription:*` 事件继续拥有录音结束、转写文本和转写错误的用户可见结果，不把 transcript 同步塞回 stop command。
- 更新生成契约、IPC 输出分类和命令说明；不保留返回空字符串的兼容形状。

## 替代方案

- 保留空字符串作为成功标记：拒绝。它没有可区分状态或调用方，成功/失败已由 IPC 的 resolved/rejected 区分。
- 让命令等待并返回 transcript：拒绝。转写由受管后台任务异步执行；等待会重新把网络处理时延带回录音 overlay 生命周期，并与事件结果重复。

## 影响与验证

成功响应从空字符串改为无正文 unit ack；命令参数、错误语义、录音停止顺序、异步调度、事件 payload 与持久数据不变。UI 当前不读取成功响应，因此无需调用逻辑或用户可见行为调整；数据库和配置无需迁移或重置。

验证：重新生成 IPC contract；运行 Rust workspace fmt/check/Clippy/tests，UI check/tests/build，IPC command/event checks、ADR index 与 diff checks。

## 回滚

若回滚，需同时恢复 Rust `Result<String, String>`、空字符串成功分支和 generated response type；事件流程、数据库与配置无需回滚或重置。
