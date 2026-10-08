# ADR 0753：Tool result hint 只按文本渲染

## 状态

已采纳并实施。

## 背景

`ToolResult.output` 保持异构 JSON。`ToolResultCard` 从任意结果 record 读取可选 `hint` 并直接插入文本节点；builtin producer 当前将它作为字符串输出，但 MCP、Skill 或未来 producer 的动态 JSON 可能在同名字段放入对象、数组或数字。该值属于共享卡片壳的展示 props，不受各 builtin renderer 的专用 shape guard 统一约束。

## 决定

- 共享卡片壳只展示字符串 `hint`；其它动态值不作为提示文本渲染。
- 不因此拒绝或改写 ToolResult payload。专用 renderer 与 JSON fallback 仍可展示原始动态结果。

## 影响与验证

只收窄 UI 展示入口，不改工具 producer、ToolResult wire 或持久化。新增组件回归用例验证 malformed object hint 被隐藏，同时有效 builtin 结果仍由专用 renderer 展示。

## 回滚

恢复对动态值的直接文本插值即可。没有 IPC、数据或持久化迁移。
