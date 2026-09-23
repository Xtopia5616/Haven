# ADR 0237：定时任务授权入口收口

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 定时任务确认与授权调用路径
- 关联：[ADR 0212](0212-process-services-off-tools-facade.md)、[ADR 0233](0233-session-managed-asset-lease-port.md)

## 背景

定时任务路径在 `AgentLayer` 中直接从 `ToolsManager` 取得授权请求并调用授权 engine，成为 Session/Agent 对总管对象的一个额外逃逸点。
确认队列、receipt 校验和执行位置本身已经正确，但授权入口可以收窄。

## 决定

1. `SessionSupervisor` 提供一个仅负责构造定时授权请求并调用 live authorization engine 的窄方法。
2. `AgentLayer` 只消费该方法的 decision；确认队列、receipt 校验、`execute_gated` 和工具执行顺序保持不变。
3. 本 ADR 不移动普通 ReAct tool batch 的确认逻辑，也不把交互式确认放入工具 future。
4. 完整移除 Agent 对 `ToolsManager` 的依赖继续拆成 prompt/catalog/resume/lifecycle 等独立切片。

## 影响与验证

只改变授权入口的所有权表达，不改变授权策略、风险计算、取消、超时、幂等或 IPC。`haven-agent` 465 项测试和严格 Clippy 通过。

## 回滚

恢复 `AgentLayer` 中的旧授权入口调用并删除 supervisor 方法；不涉及持久数据或 schema。
