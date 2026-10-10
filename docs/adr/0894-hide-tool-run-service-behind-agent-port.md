# 0894：通过 Agent-owned port 隔离 ToolRun 生命周期调用

## 状态

已接受并实现（2026-10-10）。

## 背景

Agent 的 `SessionSupervisor` 直接保存 `Arc<ToolRunService>`，会话生命周期、定时执行和后台结果交付因此可以调用 Tools 的具体服务。完成接收器还要求调用方传入 `ToolRunService` 才能扫描持久 outbox 和恢复定时触发，导致恢复细节也越过 crate 边界。

这不是单一方法的依赖问题：Agent 使用了会话归属、取消、定时执行 claim、完成状态写入、恢复、完成确认和 session 投影等一组用例。把 getter 改名或只藏住 service 字段，仍会让 Agent 绑定实现。

## 决定

- Agent 定义 `AgentToolRunPort`，只描述会话监督器、后台结果投递和定时执行实际需要的能力；Agent 的生产结构仅保存该 port。
- Agent 定义完成接收器 port；App adapter 把接收与 outbox 恢复调用映射到同一个 Tools owner。
- `ToolRunCompletionReceiver` 的恢复、outbox claim 与 transient broadcast 协调留在 adapter 内，Agent 只取得 typed completion。
- 测试可在 `cfg(test)` 下访问具体服务进行持久化故障注入；该入口不进入生产结构或默认 API。

## 影响

- Agent 生产代码不能依赖或导入 `ToolRunService`；同一 Tools owner 仍管理 ToolRun 状态、持久化、定时器、终态仲裁和恢复。
- 会话取消的失败语义、scheduled execution claim、background completion outbox 确认顺序和启动恢复行为保持不变；没有新增状态副本或第二个事件总线。
- App 的 runtime shutdown、ToolRun IPC 和生命周期事件接线仍通过 `ToolServices.tool_runs` 直接使用具体服务，后续需按消费者契约继续审查（路线图 §5.7）。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- 让 Agent 继续持有 `ToolRunService`，只调整字段或 getter 名称：拒绝，因为调用端仍可依赖服务实现，编译器无法保证 Agent 只使用会话所需能力。
- 再增加一个 Agent 自己拥有的 completion bus：拒绝，因为这会复制 Tools 的 durable outbox、claim 和重放职责，造成两个恢复来源。
