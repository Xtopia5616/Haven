# ADR 0756：Shell ToolResult 复用闭合执行状态

## 状态

已采纳并实施。

## 背景

Shell builtin 的后台启动结果写入固定 `execution_mode: "background"` 和 `status: "running"`。执行模式来自 Rust `ToolExecutionMode`；status 来自 Common `ToolRunStatus`。`ToolShellResult` props 和 ToolResult registry guard 却分别重复声明执行模式值，并将 status 放宽为任意字符串，使未知状态仍能进入 Shell 专用 renderer。

## 决定

- UI shell renderer 使用 `toolResultPresentation.ts` 的 `ToolExecutionMode` tuple、派生类型与 runtime guard，组件 props 和 registry 共用同一值源。
- Shell `status` props 复用 generated `ToolRunStatus`；registry 使用其 generated value guard。`execution_mode`、`status` 与其他可空 optional output fields 的 props 明确允许 null，与当前 shape guard 一致。
- 不把 ToolResult output 变成全局静态 schema，也不生成额外 Tauri contract。

## 替代方案

保留两处独立的执行模式字符串列表会留下漂移面；继续将 status 保持开放字符串则允许未知值穿过已知 builtin renderer。为整个异构 ToolResult 建立统一 DTO 会越过逐工具 output 的动态边界。

## 影响与验证

只收紧 UI shell renderer props 与 registry validation，不改 Shell producer、ToolResult wire、IPC 或持久化。测试覆盖合法后台运行结果保留专用 renderer，以及未知 status 回退 JSON；UI check 与 test:run 通过。

## 回滚

恢复独立的执行模式字符串判断与开放 status props/guard 即可。没有 IPC、数据或持久化迁移。
