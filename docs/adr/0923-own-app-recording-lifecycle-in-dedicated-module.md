# 0923：将 App 录音生命周期协调器归入独立模块

## 状态

已接受并实现（2026-10-11）。

## 背景

`app_state.rs` 是 App composition root，负责装配 `AppState`、配置依赖和启动后台工作。它还定义了 `RecordingLifecycleOwner` 与 guard：这套状态只服务用户发起的录音命令与 Shell handler，用互斥锁串行化 start/stop/cancel，并持有当前 `rec-*` identity。两个并发语义测试也位于 AppState 测试模块中。

这使录音协调机制的实现和测试归属到全局状态装配文件。录音 lifecycle 是一个有自身 owner、并发约束和消费者边界的 App 子域，应由专属模块命名和维护。

## 决定

- 将 `RecordingLifecycleOwner`、`RecordingLifecycleGuard` 与它们的并发测试移至 `recording_lifecycle.rs`。
- `AppState` 仍在组合根创建并持有唯一实例；recording commands 与 Shell handler 继续共享它。
- 将 owner 与 guard 限制为 crate 内可见，不对其他 crate 或默认根 API 暴露。
- 保持 timed `media.record` 与 App voice recording identity 分离。

## 影响

只调整模块所有权与测试位置。锁的范围、`rec-*` 生成时机、stop/cancel identity detach 顺序、事件、IPC 和持久化均不变；无数据迁移或重置要求。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-app-binary`
- 全 workspace `cargo check`、`cargo test`、严格 Clippy 与 ADR 索引检查
- 静态搜索确认协调器实现与测试只位于专属模块，调用方共享 AppState 创建的同一实例。

## 替代方案

- 保留实现于 `app_state.rs`：拒绝，因为该模块会继续同时承担 composition 与录音并发策略。
- 为两个同 crate 消费者新增 trait object：拒绝，因为它们需要同一具体协调器实例，trait 只会增加无独立替换语义的抽象层。
- 将录音命令和 Shell handler 合并：拒绝，两者的宿主职责不同；它们只应共享 lifecycle owner 和 identity 交接。

## 回滚

将新模块类型和测试移回 `app_state.rs`，恢复对应 imports 与模块声明即可。无外部接口、持久化或 wire 变化。
