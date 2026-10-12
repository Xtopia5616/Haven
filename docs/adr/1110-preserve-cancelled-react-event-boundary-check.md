# ADR 1110：取消退出时完成 ReAct event boundary 核验

## 状态

已接受（2026-10-13）

## 背景

ReAct 的取消退出路径需要读取 durable event cursor，确认 `session_events` 在 handler 返回前仍可恢复。该只读核验原样继承已取消的 turn token；`run_blocking_cancellable` 会竞速 SQLite 读取和取消信号，因而偶发地把成功的取消退出改成 event-boundary 错误。复现测试在 workspace 并行运行时失败，单独重跑时通过，证实了时序竞争。

## 决定

- `exit_cancelled` 在最终 event cursor 核验中不传递已取消的 turn token；同一边界 helper 的常规调用仍保留其现有取消 token。
- 测试使用已取消 token 直接验证边界核验仍成功，并确认不写入或改动 event log。

## 替代方案

将取消引发的 cursor 读取错误按边界验证失败处理会把预期取消升级成错误状态；静默忽略失败又无法区分数据库错误。取消退出的最后只读完整性核验必须有机会完成，因此该调用使用非取消读取。

## 影响

只改变 ReAct 取消退出时的只读 cursor 检查行为；event 写入顺序、持久格式、IPC 与其他取消任务不变。若 SQLite 读取本身失败，仍按原规则返回边界错误。

## 验证

- `cargo test --locked -p haven-agent cancelled_run_still_verifies_its_final_event_boundary -- --nocapture`
- `cargo test --locked -p haven-agent run_session_cancelled_mid_batch_surfaces_interrupted_tools -- --nocapture`
- 全工作区 test/check/clippy 门禁。

## 回滚

回滚时不得重新将已取消的 turn token 用于取消退出的最终 cursor 检查；任何替代实现必须保留已取消 token 下成功读取 durable cursor 的回归覆盖。
