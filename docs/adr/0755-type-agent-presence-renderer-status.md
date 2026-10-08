# ADR 0755：Agent presence renderer 状态复用闭合值

## 状态

已采纳并实施。

## 背景

Messaging 的 `AgentInfo.status` 使用 Rust `AgentStatus`，只序列化为 `online` 或 `offline`。内置 Agent `ToolResult.output.agents[]` 是动态 JSON，不经过 generated Tauri contract；但 `ToolAgentResult` props 和 renderer guard 都把实际展示的 `status` 声明为任意字符串，因此未来未知值会被当作状态标签展示。

## 决定

- UI Agent presence renderer 共用 `ToolAgentPresenceStatus` 与其值守卫；当前值域对应 Rust `AgentStatus` 的小写 Serde 表示。
- 列表 row 必须具有字符串 `name` 和已知 `status`；`title` / `role` 可缺省或为 null。`last_seen`、`started_at`、`parent` 与 `capabilities` 不由 renderer 消费，不参与专用 view 的校验。
- 未知/缺失状态回退到原始 JSON；不为异构 `ToolResult.output` 建全局 schema 或 generated IPC 类型。

## 替代方案

将该 ToolResult 家族改为 generated/全局静态 schema 会错误地收窄动态工具输出边界，并使其它工具输出共享不相关的 contract。只继续使用开放字符串则无法让 renderer 对齐其闭合 producer。

## 影响与验证

只收窄 UI Agent renderer presentation props 和 guard，不改变 Rust producer、ToolResult JSON、Tauri IPC 或持久化。回归测试覆盖未知状态回退、有效状态保留专用 renderer，以及非展示字段不影响判断；UI check 与 test:run 通过。

## 回滚

恢复开放字符串 status 与原 guard 即可。没有 IPC、数据或持久化迁移。
