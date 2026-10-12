# ADR 1106：将 Agent 后台消费者纳入关闭生命周期

## 状态

已接受（2026-10-13）

## 背景

`AgentLayer::start_after_memory_ready` 启动会话生命周期事件、ToolRun 结果和定时完成消费者，以及 ToolRun 恢复任务。这些任务收到应用的取消令牌，但原先丢弃了 `JoinHandle`。`ApplicationRuntime::shutdown` 因此无法确认消费者退出后再关闭 session、ToolRun 和 MCP 资源。`replace_router` 的 provider health check 也以 detached task 运行，且没有明确 deadline。

## 决定

- `AgentLayer` 以一个有界关闭期的 task group 持有后台消费者的 `JoinHandle` 和取消令牌；任务正常完成后从 registry 移除，任务 panic 会留下诊断并释放 registry 项。自动标题任务也在应用关闭时关闭注册入口、取消并 join。
- `ApplicationRuntime::shutdown` 先取消应用 token、停止输入生产者，再 cancel 并 join Agent task group，之后才清理会话运行态及关闭 ToolRun/MCP 服务。
- 关闭期内未完成的 Agent task 在五秒宽限后 abort 并 join，避免依赖资源关闭后仍有 detached consumer。
- router prewarm 也由 task group 持有，受取消信号控制，并使用五秒 health-check deadline。
- task group 在 shutdown 开始后拒绝新任务注册。

## 影响

Agent 消费者生命周期由 AgentLayer 持有，并在其依赖资源关闭前结束。短期 router prewarm 不会无限等待。没有数据库 schema、配置或 IPC shape 变化，无需重置持久数据。

## 验证

- Agent 单元测试注册一个等待取消的消费者，确认 shutdown 会取消并等待其清理逻辑结束，且 shutdown 后拒绝新任务。
- `cargo check --locked -p haven-agent -p haven-app-binary`。

## 回滚

不得重新丢弃长期消费者的 JoinHandle。若 task group 的存放位置调整，必须保证 shutdown 在关闭 session、ToolRun 和 MCP 依赖前 cancel 并 join 所有 Agent 后台消费者。
