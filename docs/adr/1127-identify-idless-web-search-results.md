# ADR 1127：为无 provider ID 的搜索结果分配 renderer 身份

## 背景

`web_search_return_effects` 会从 provider 聚合响应中提取带引用内容的完成结果。部分 adapter 响应没有非空 provider item ID；Agent 仍能发送 `agent:web_search` 事件，但 renderer reducer 以前要求 `call_id`，因此直接丢弃这些引用卡片。`call_id` 是外部 provider 值，不能为了满足 UI 身份要求而伪造或改写。

## 决定

1. `AgentWebSearchEvent` 增加可选 `call_index: u32`，仅在聚合完成项没有非空 provider ID 时填写。其值是该项在本次响应 `web_search_calls` 列表中的零基位置。
2. 非空 `call_id` 继续逐字保留；同一事件不同时携带有效 `call_id` 和 `call_index`。App DTO、生成的 wire contract 与 UI mapper 共用该字段，mapper 校验 u32 范围和互斥关系。
3. WebSearch renderer key 为 provider ID 与 response index 使用不同 namespace，避免 provider 任意字符串与 index fallback 碰撞。Reducer 接受任一种身份，因此 id-less 聚合引用结果可完成投影。
4. 没有 provider ID、也没有聚合 response index 的流式状态仍不单独创建卡片；完成聚合结果拥有独立身份与引用正文。

## 替代方案

- 伪造 `call_id`：会把 UI fallback 冒充 provider wire identity，并污染跨层诊断。
- 按 action 或正文生成身份：同一 turn 可有多项相同 action/引用，内容相等不能作为实体身份。
- 在 UI 为无 ID 事件 mint 随机值：无法由后续事件稳定复用，也绕过后端事件契约。

## 影响

IPC 事件增加一个可选数字字段，provider event JSON 与持久 transcript 不变。结果卡 renderer key 仅用于当前 UI 投影，不是数据库实体 ID。无需数据迁移或 reset。

## 验证

- Agent 测试覆盖多个无 ID 完成项得到不同 index，并验证非空 provider ID 不附带 index。
- UI 测试覆盖 call-index mapping、非法范围/身份冲突拒绝、无 ID 结果投影及 provider/index key namespace 不碰撞。
- 运行 IPC contract generator check、workspace Rust tests/clippy/fmt，以及 UI type check 和全量单测。

## 回滚

回滚事件字段、UI mapper/reducer fallback 和 index namespace 即可；未持久化任何新身份，不需要数据清理。
