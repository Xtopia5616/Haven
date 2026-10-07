# ADR 0744：Tool result envelope metadata 复用闭合类型

## 状态

已采纳并实施。

## 背景

`ToolResultEnvelope` 是工具终态结果附带的 Agent/UI 元数据，但 `outcome`、`error_class`、`retryability` 与历史 wire 字段 `retry_safety` 曾以字符串表示。UI 事件 mapper 需要维护另一套手写值清单。更重要的是，`ToolResult::envelope` 曾从成功工具的动态 `output` 复制 `retry_safety`；MCP 或其它外部工具输出可以提供同名值，覆盖 operation policy 给出的权威幂等性标签。未知字符串还会使整个 observation event 被 UI mapper 丢弃。

两个名字相近的 closed vocabulary 表示不同概念：`OperationIdempotency` 表示操作策略的 `idempotent` / `non_idempotent` / `unknown`；`ToolRetrySafety` 表示工具目录中的 `safe_to_retry` / `unsafe_to_retry` / `unknown`。`retry_safety` 是已存在的 observation envelope wire key，其值一直使用前一种词汇，不能据字段名改成后一种。

Durable `session_events.payload` 中的 `StoredObservationUi` 保存该 envelope。历史 payload 可能含有任意字符串，因为它过去可由工具输出填充；旧事件重放必须能读取这些记录。

## 决定

- 将 `ToolExecutionOutcome`、`ToolErrorClass`、`ToolRetryability` 与 `ToolResultEnvelope` 放在 Common，Agent、App 和 Tools 直接共用这些类型。
- envelope 的 `outcome`、`error_class`、`retryability` 使用 Common enum；`retry_safety` 使用 `OperationIdempotency`。Rust 生成 TypeScript enum 和值清单，UI mapper 对当前新 wire 值执行闭合校验。
- `ToolResult::envelope` 只从显式传入的 operation policy 填充 `retry_safety`，不读取动态工具输出里的同名属性。工具输出仍是开放的 `Value`；该字段不参与 retry authorization。
- 为 durable envelope 的 `retry_safety` 单独使用兼容反序列化：`idempotent` 与 `non_idempotent` 保持原义，其它历史字符串读作 `unknown`；重新序列化时使用 canonical enum 字面量。其它 envelope enum 仍严格解析。

## 替代方案

- 继续从工具输出复制 `retry_safety`：拒绝。外部执行结果不能覆盖由可信 operation policy 提供的 envelope metadata。
- 把该字段改成 `ToolRetrySafety`：拒绝。会改变现有 wire vocabulary，并混淆操作幂等性与工具目录的重试安全类别。
- 对 durable payload 中所有未知 `retry_safety` 字符串严格失败：拒绝。历史成功输出可能写入任意字符串，严格失败会阻断已有 session event 的恢复。
- 将整个工具 `output` 收窄为同一个 envelope enum：拒绝。MCP、Skill 与 builtin 工具结果仍是异构动态 JSON，输出 schema 审计另行进行。

## 影响与验证

新 observation envelope 的 JSON 字段和值不变；`retry_safety` 始终取自可信 operation policy。旧 durable annotation 中不认识的字符串会在读取后规范化为 `unknown`。不改变 SQLite schema、canonical event 写入、retry authorization 或工具输出 JSON，不需要数据库迁移或重置。回归测试覆盖 enum round-trip、历史未知值兼容及输出同名字段不能覆盖 operation policy；IPC event gate 确认 Common type ownership 和 generated UI guard。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过；UI `check`、`test:run`（125 files / 993 tests）、`build` 通过；IPC contract 检查（80 handlers，含 generated TypeScript `--check`）、IPC event 检查（35 channels）、ADR index（727 records）及 `git diff --check` 通过。

## 回滚

如回滚，将 Common envelope 与 enum 恢复为 Tools 定义的字符串结构，并恢复 UI 手写 result 值列表；同时移除从可信 operation policy 投影 `retry_safety` 的保护。JSON schema、数据库及 durable payload 无需重置。
