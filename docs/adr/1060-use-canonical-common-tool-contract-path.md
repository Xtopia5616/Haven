# ADR 1060：统一 Common 工具契约导入入口

## 状态

已接受（2026-10-12）

## 背景

`haven_common::tools` 是工具定义、策略 enum 和结果 envelope 的所有者，但 `haven_tools::tool_contract` 又将它们重导出，`haven_tools` 根继续公开同一批类型。Agent 和 App 因而从 Tools crate 导入由 Common 定义的 `ToolExecutionOutcome`、`OperationIdempotency`、`NetworkAccess` 等契约，形成第二个公共入口，并模糊类型 owner。

## 决定

- Common 工具 contract 统一通过 `haven_common::tools` 导入。
- `haven_tools` 根只公开 Tools 所有的执行接口、策略组合、结果和生命周期能力；Common contract 的 crate 内兼容导入改为 `pub(crate)`，不形成外部 API。
- `haven_tools` 自有的 `Tool`、`ToolResult`、`OperationPolicy`、`ToolExecutionMode` 和执行/适配能力继续从 crate root 导出。

## 影响

Common DTO 和 enum 不再有 Tools crate 的第二个公开路径；运行时序列化、数据字段和 tool behavior 不变。下游需要显式从 Common 导入这些 contract。

## 验证

- 搜索确认 App、Agent 等消费者不再通过 `haven_tools` 导入 Common 工具 contract。
- Workspace `cargo fmt`、`cargo check`、`cargo clippy` 和 `cargo test` 通过。
- ADR 索引与本地链接检查通过。
