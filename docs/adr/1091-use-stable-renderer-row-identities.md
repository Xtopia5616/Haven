# ADR 1091：列表 renderer 使用稳定行身份

## 状态

已接受（2026-10-13）

## 背景

MemoryRecall 曾把 `entityId` 与可变展示文本拼成 Svelte key；重复实体或文本更新会改变 renderer 行身份。UI Automation `elements` 列表使用可重复的显示名称作为 key，而 Rust producer 已提供 `element_token`。

## 决定

- MemoryRecall 的 Svelte key 使用 canonical `entityId`；命令 parser 拒绝同一 recall 响应中的重复实体 ID。
- UI Automation 使用 producer 提供的 `element_token` 作为 key。旧结果没有 token 时，按列表索引作为仅用于渲染的 fallback key；名称只用于显示。
- `element_token` 是窗口结果 payload 的可选字符串字段；若提供非字符串值，builtin renderer validation 拒绝该 payload。

## 影响

行身份不再依赖展示文案；相同名称的按钮可并存。旧 UI Automation 结果仍可显示。

## 验证

Memory contract 测试覆盖重复实体 ID；ToolResultCard 测试覆盖重复元素名称和 token 类型校验；运行 UI 全量测试与 Svelte check。

## 回滚

不得用展示文本恢复实体列表身份，也不得让同一记忆响应把重复实体交给 keyed renderer。
