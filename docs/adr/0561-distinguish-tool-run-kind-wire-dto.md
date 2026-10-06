# ADR 0561：区分 Tools ToolRunKind 与 App ToolRunKindDto

## 状态

已采纳并实施。

## 背景

`haven_tools::ToolRunKind` 是 Tools runtime 中用于执行、取消、恢复和分类 ToolRun 的枚举。App `events.rs::ToolRunKind` 则独立定义 `Background/Scheduled`，并用于 `ToolRunEvent` 与 Tauri `cancel_tool_run`、`list_tool_run_history` 参数；它负责 App wire 序列化和 TS 合同。App 源码注释已经明确表示二者虽值相同但必须分开，然而名称相同隐藏了边界，生成 TypeScript 契约也因此只暴露一个泛称。

## 决定

1. 将 App wire enum 改名为 `ToolRunKindDto`；Tools runtime enum 保留 `ToolRunKind`。
2. 重新生成 IPC TypeScript 类型为 `ToolRunKindDto` / `ToolRunKindDtoInput`，并由前端 `contracts/toolRun.ts` 将生成 wire 类型映射到 UI 本地 `ToolRunKind`。
3. 保持 `background` / `scheduled` 字符串值、JSON 字段 `kind`、Tauri 命令名与参数、事件 payload shape、校验、授权和 ToolRun 行为不变。

## 替代方案

- 复用 Tools 的 `ToolRunKind` 并删除 App DTO：拒绝。这样会把 App 的 IPC/event SerDe 与演进责任耦合到 Tools runtime 类型，违反代码现有边界决定。
- 保持同名，只靠注释说明：拒绝。跨 crate 搜索和调用点仍无法从符号上辨认 runtime 与 wire 角色。
- 通过改 JSON 变体或字段值来区分：拒绝。现有 wire 值准确且无需变更。

## 影响与验证

- 修改 App Rust 类型名和生成 TypeScript 类型名；序列化后的 JSON 不变。
- 验证：`scripts/generate-ipc-contracts.ps1`、`scripts/check-ipc-contracts.ps1`、`scripts/check-ipc-events.ps1`、Rust workspace fmt/check/strict Clippy/tests、UI check/tests/build、ADR 索引检查均通过。

## 回滚

将 App 类型及生成 TS 名恢复为 `ToolRunKind` 并更新前端 alias；无需持久化迁移。
