# ADR 0746：ToolRun board 输出复用生成状态类型

## 状态

已采纳并实施。

## 背景

`ToolRunsTool` 的列表由 `ToolRunListView` 生成；每个列表项都有实体 `tool_run_id` 和必填的 Common `ToolRunStatus`。UI 的 `ToolRunSummary` 却把两者都声明为可选字符串，renderer validator 也只检查行内状态是字符串。由此，未知状态会被当作实体状态标签展示。

单项 inspect 的 root `status` 有不同语义：它还可以是 `not_found`，所以不能把整个动态输出对象统一收窄到 `ToolRunStatus`。

## 决定

- 列表项 `tool_run_id` 和 `status` 在 renderer props 中均为必填，`status` 直接引用 generated `ToolRunStatus`。
- 将 `contracts/toolRun.ts` 的 `isToolRunStatus` 作为领域校验器导出；builtin renderer registry 用它拒绝缺失或未知的列表状态。
- 形状无效时仍回退 `ToolJsonResult` 并显示原始 JSON；root status 与其它 ToolResult 字段继续保持各自动态语义。

## 影响

只收紧 UI builtin presentation contract。Rust producer、ToolResult 的异构 JSON、provider wire、持久化和 MCP/Skill 扩展均不变。

## 验证

- 负向 renderer 用例覆盖列表行未知状态并验证 JSON fallback。
- 正向 renderer 用例覆盖生成状态 `running`。
- UI 类型检查与全量测试通过。

## 回滚

可恢复列表行的开放字符串状态与原 validator。没有数据、IPC 或持久化迁移。
