# ADR 0723：Live usage event 复用 LlmCallKind

## 状态

已采纳并实施。

## 背景

`LlmCallKind` 已在 Common 中限定 `agent`、`media`、`tool`，Agent runtime 输入和 SQLite `llm_usage.call_kind` CHECK 也使用同一值域。可是在 live `agent:usage` 链路中，`UsagePayload`、`AgentEvent::Usage` 和 App `AgentUsageEvent` 逐层将它转为 `String`。UI 又手写相同 union，wire mapper 只检查 `string` 并断言为该 union；聊天 handler 再校验一次，但无效值测试却要求接受 `future_call_kind`。

ADR 0271 曾为了保持旧 IPC 字符串边界，明确保留这份宽松契约。当前测试版不要求 wire 向下兼容；Serde enum 会继续生成相同的 snake_case JSON 字符串，因此可以移除该运行时双重契约。数据库读出的 `LlmUsageRecord.call_kind` 是不同的持久化投影，仍由 Memory 在重新进入 runtime 时解析。

## 决定

- Agent `UsagePayload` / `AgentEvent::Usage` 与 App `AgentUsageEvent` 都直接使用 `LlmCallKind`。
- IPC generator 显式导出 Common enum，生成 `LlmCallKind` 和 `LLM_CALL_KIND_VALUES`；UI live payload 类型与运行时 mapper validator 都引用该生成来源。
- 删除聊天 handler 对同一值域的第二次校验；unknown 值在 Agent event mapper 边界拒绝。
- 保持 JSON 值 `agent` / `media` / `tool` 和所有字段名不变。数据库 `LlmUsageRecord.call_kind` 继续是字符串，不改 schema、持久化值或恢复逻辑。

## 替代方案

- 保留 live wire 字符串并手写 UI union：拒绝。闭合值域已经由 runtime enum 和 schema 唯一限定，继续复制会保留不必要的第二契约。
- 把 durable DB read DTO 一起改成 enum：拒绝。SQLite 查询模型拥有字符串字段，Memory 仅在其 runtime 聚合边界解析；它不是 live event 的 DTO。

## 影响与验证

前端静态事件类型从手写 union 转为 generated `LlmCallKind`；非法 `call_kind` 在唯一事件 mapper 边界被丢弃。合法 IPC JSON、数据库数据和会话恢复都不变，无需重置。Rust event bridge 测试验证 Serde 输出，UI contract 测试覆盖合法映射和 unknown 拒绝。

按跨端 event contract 门禁运行 Rust workspace fmt/check/Clippy/tests、UI check/tests/build、IPC generator/drift 与 event checks、ADR index 和 diff checks。

## 回滚

恢复 live event 的 `String` 字段和宽松 validator，并同步 UI union 与 handler 校验；数据库和持久化数据不涉及回滚。
