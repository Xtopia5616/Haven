# 0895：通过 App-owned port 调用 ToolRun 生命周期

## 状态

已接受并实现（2026-10-10）。

## 背景

`ApplicationRuntime` 原样保存完整 `ToolServices` bundle。ToolRun IPC、关机和生命周期事件接线因此直接调用 `ToolRunService`；App runtime 也持有 App 并未使用的 MCP config、asset 和 live service 等字段。

Agent 已用消费端 `AgentToolRunPort` 隔离会话执行、取消和 completion recovery，但 App 命令仍直接绑定同一具体实现。单独给 Agent 加 port 并不能形成跨层的服务边界。

## 决定

- `ApplicationRuntime` 不再保存 `ToolServices`，改持 App 实际使用的 `AppServices` 视图。
- App 定义 `AppToolRunPort`，涵盖 UI board/history/cancel/delete、生命周期 sink 和 shutdown；Tools 的具体 service 仅由 composition adapter 持有。
- Agent 继续使用独立的 `AgentToolRunPort`，由 Agent adapter 映射到同一个 ToolRun owner；两个消费端不共享宽泛的万能 port。
- `schedule` 仅作为 App test 构造路径的 `cfg(test)` 操作，不进入生产 App port。

## 影响

- ToolRun 状态、持久化、终态仲裁、生命周期事件顺序和 outbox 恢复仍由唯一 `ToolRunService` 拥有；App adapter 不复制状态。
- App 命令和 runtime 不再依赖具体 ToolRun service 类型。`AppServices` 中 MCP manager、SkillRegistry、SkillRunner 与 live-output 仍是具体 handle，其他 `ToolsFacade.share_services()` 资产调用也仍存在；这些继续按路线图 §5.7 审查。
- 本决定收窄 App Runtime 持有的 service surface，不改变 Tauri IPC、存储字段或用户可观察行为。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- 让所有 App 代码继续持有 `ToolServices`：拒绝，因为一个消费者可访问与自身无关的全部具体 service handle，调用边界无法由类型检查。
- 将 Agent 和 App 合并成一个 ToolRun 通用 port：拒绝，因为两者需要的用例不同，宽接口会重新暴露不相关操作。
