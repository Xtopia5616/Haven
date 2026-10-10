# 0899：通过按消费者划分的 capability ports 隐藏 ToolRunService

## 状态

已接受并实现（2026-10-10）。

## 背景

Agent 与 App 已有消费端 ports，但 `ToolServices.tool_runs` 仍公开 `Arc<ToolRunService>`。组合适配器因此能绕开 Tools 的领域边界，直接调用状态机。完成接收器也要求调用方把 `ToolRunService` 传回去做 scheduled/outbox 恢复；Agent 的跨 crate 测试构造器同样暴露具体 service。消费端接口虽然存在，具体实现仍然泄漏到边界之外。

## 决定

- 从 `ToolServices` 移除具体 `tool_runs` 字段，改为按实际消费者划分 `tool_run_agent` 与 `tool_run_management` capability ports。
- Tools 内部适配器持有唯一 `Arc<ToolRunService>`，实现会话交付/调度与应用管理能力；App 和 Agent 适配器只依赖这些接口。
- 完成接收器自身持有 Tools 内部恢复能力，调用方只接收完成结果，不再提供 service 参数。
- 从默认跨 crate API 隐藏 `ToolRunService` 与 `ToolRunCompletionReceiver`；具体实现继续由 Tools、其 builtin 装配和内部测试使用。
- 将 `ToolRunService::set` 改名为 `schedule`；scheduled run 的创建职责使用明确动词，不再与配置写入或 operation 名 `schedule.set` 混淆。Builtin composition context 与 shell/schedule/tool-runs provider 不再公开具体 service 字段。
- Agent/App 跨 crate 测试通过 `haven-tools/test-support` 下的 `ToolRunTestSupportPort` 注入 store、安排任务、观察状态和取消任务；测试接口不进入默认生产 API。
- 保留 Agent 的消费端 `AgentToolRunPort` 和 App 的消费端 `AppToolRunPort`，由 composition adapter 显式映射到 Tools owner capability。它们代表使用方边界，不再承载或泄漏 service 实现。

## 影响

- ToolRun 状态、持久化、timer、completion bus、outbox 轮询与 scheduled fire 的恢复仍只有 Tools 一个 owner；运行时行为和 completion 顺序不变。
- IPC DTO、SQLite/schema、session events 和安全语义不变，无需数据库或配置重置。
- 对 `ToolServices` 字段和默认公开类型的 Rust API 是破坏性收口；仓库无兼容要求，调用方须改为 capability。
- 其他 builtin provider 中的 `ToolRunService` 引用仍属于 Tools crate 内部装配边界；它们通过 crate-private context 与字段接收 service，不再形成跨 crate 入口。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`

以上门禁均于 2026-10-10 在固定 Windows 工具链通过。workspace 测试通过；Agent 619 passed、Tools 805 passed、Memory 408 passed，另有仓库中已有的 ignored 测试未运行。Clippy 使用 `-D warnings`，无警告。

## 替代方案

- 只保留 Agent/App 消费端 ports，继续把具体 service 交给 adapter：拒绝，因为消费端边界仍无法阻止其他跨 crate 调用者直接绕过 Tools owner。
- 给所有消费者共用一个全能 ToolRun port：拒绝，因为它会把会话交付与 App 历史/IPC 管理能力混为同一个权限面。
- 把完成恢复移入 Agent：拒绝，因为 Agent 不拥有 ToolRun bus、durable outbox 或其恢复和 claim 语义。
