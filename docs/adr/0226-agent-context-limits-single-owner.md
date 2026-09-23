# ADR 0226：Agent context limits 单一运行时 owner

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 中 `ContextLimitsConfig` 的运行时更新与读取
- 关联：[ADR 0068](0068-versioned-config-service.md)、[ADR 0079](0079-context-compaction-budget-and-degradation.md)

## 背景

`AgentLayer` 和 `ReActEngine` 都保存了 `ContextLimitsConfig` 的运行时副本。设置变更时需要同时更新两份状态；调用方再从 `AgentLayer` 读取通知摘要长度，可能与 ReAct 实际使用的限制发生漂移。

## 决定

1. `ReActEngine` 是 agent 层 context limits 的单一运行时 owner。`AgentLayer` 不保存该配置副本；`set_context_limits` 只转发给 ReActEngine，`limits()` 直接读取 ReActEngine 当前值。
2. `ConfigService` 仍是配置来源。应用设置保存后，将解析出的 `ContextLimitsConfig` 应用于 agent 运行时；本 ADR 不改变配置持久化或加载流程。
3. `AgentLayer::new` 仍使用传入配置初始化 `MemoryService`、`MemoryWorker` 和 `ReActEngine`。其中 `MemoryService` / `MemoryWorker` 的启动构造快照及其后续热更新语义不在本切片迁移范围内。
4. 通知摘要和 action 结果通知继续经 `AgentLayer::limits()` 读取 `notification_summary_chars`；摘要截断与消息内容保持不变。

## 替代方案

- 保留 `AgentLayer` 与 `ReActEngine` 两份副本：继续承担双写和状态漂移风险。
- 将运行时读取移到 `ConfigService`：会扩大本切片到配置服务接入与应用生命周期的范围，且 ReActEngine 仍需要自己的执行配置快照。

## 影响

Agent 层只有 ReActEngine 持有可热更新的 context limits 运行时状态。`AgentLayer` 的读取与 ReActEngine 一致；MemoryService / MemoryWorker 构造时接收的初始化参数和现有通知行为不变。

## 验证

- 定向单测更新 `notification_summary_chars`，并验证 `AgentLayer::limits()` 与 `ReActEngine::limits()` 一致。
- `cargo fmt -p haven-agent -- --check`
- `cargo check --locked -p haven-agent`
- `cargo test --locked -p haven-agent`
- `cargo clippy --locked -p haven-agent -- -D warnings`
- `git diff --check`

## 回滚与重置

无持久化数据或配置格式变化，不需要数据重置。回滚时恢复 `AgentLayer` 的运行时副本和双写逻辑，并移除此 ADR 与目录索引项。
