# ADR 0345：ToolsManager 授权策略与执行边界

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools` 的授权请求准备与工具执行入口
- 关联：[ADR 0211](0211-operation-registry-and-platform-snapshot.md)、[ADR 0212](0212-process-services-off-tools-facade.md)、[ADR 0257](0257-explicit-tool-catalog-injection.md)、[ADR 0326](0326-tools-runtime-capability-resolution.md)、[ADR 0331](0331-tools-manager-capability-snapshot.md)、[ADR 0333](0333-tools-runtime-coordinator.md)

## 背景与审计

当前 `ToolsManager` 实现分布在 `manager.rs`、`execution.rs` 和 `catalog.rs`。运行时 allow/deny/confirm 决定只由 `AuthorizationEngine` 作出：Agent、app 命令和 scheduled action 调用方先创建 `AuthorizationRequest` 并评估，再调用执行入口；`AuthorizedExecutor` 不在工具 future 中请求确认。

授权请求准备仍和执行入口一起放在 `execution.rs`。live session lookup 与 `ToolCatalogSnapshot` 分别包含一份“工具不存在时”的默认 `OperationPolicy`，而 canonical authorization input 也由执行器方法和 snapshot 请求路径分别处理。它们表达同一类请求策略，但不拥有 allow/deny 状态。

审计没有发现 session overlay、asset lease 与 catalog projection 重复维护权威事实：

- `SessionCatalog` 唯一持有 session tool registrations 与版本；`OperationCatalog` / `ToolCatalogSnapshot` 从 installed registry 与该 overlay 读取并生成只读投影。执行时仍按 session overlay 优先、installed registry 后备的顺序查找。
- `ManagedAssetRegistry` 唯一持有 managed asset、pending lease 与 session lease 状态。`ToolsManager`、`ToolServices` 和 builtin 克隆共享同一内部 registry 状态；session cleanup 与 retention GC 继续经原 lease release 路径。
- `ToolRuntimeCoordinator`、`ToolCapabilitySnapshot` 及 ActionService 的 Store/lease/completion 边界已经分别收口，本 ADR 不重复提取这些 owner。
- MCP/Skill loader 使用同一 session catalog 写入 overlay；HTTP 的 live network policy 仍在 `AuthorizationEngine` 与 `HttpTool` 原边界。

## 决定

1. 新增 crate-private `ToolAuthorizationPolicy`，从 live session lookup 或不可变 turn catalog 准备 operation policy、canonical authorization input 和 typed `AuthorizationRequest`。
2. 未知工具的 conservative fallback 由该 policy 提供；`ToolCatalogSnapshot::operation_policy` 和 live authorization request 共用同一 fallback。
3. `AuthorizationEngine` 保持唯一 allow/deny/confirmation 决策 owner。各调用方继续在 `execute_tool` 前实时评估请求；缺少或失效 confirmation receipt 仍 fail closed。
4. `AuthorizedExecutor` 继续只负责 execution admission、enabled/circuit gate、input validation、执行、retry 和 result classification。Tool selection、session overlay、asset lease lifecycle、catalog snapshot/projection、runtime capability、MCP、Skill、HTTP 和录音行为均留在原 owner。
5. 保留所有既有 public method signature、IPC、配置、数据库、ID、capability semantics、错误文本、日志内容与顺序。live policy/input 仍使用原 session-first lookup，snapshot request 仍使用原 immutable turn view。

## 替代方案

- 继续将授权 request preparation 留在 execution module：会保留执行 facade 与“执行前 policy context”共置，并保留两份未知工具 fallback，拒绝。
- 把实际 authorize decision 移入工具 future：会破坏交互确认先于执行、scheduled authorization 和失效 receipt fail-closed 边界，拒绝。
- 再拆 SessionCatalog、ManagedAssetRegistry 或 OperationCatalog：审计未发现重复权威事实；增加 facade/adapter 只会移动复杂度，拒绝。
- 搬迁 MCP/Skill/HTTP 和 runtime capability owner：会扩大至独立连接、session lifecycle 和 live network gate，超出本次审计。

## 影响与验证

- 无 schema、配置、IPC、provider contract 或数据变化，无重置要求。
- 新增回归测试覆盖 session overlay tool 的 canonical input 与 operation policy 在 live 和 snapshot 路径一致，以及未知工具 fallback 一致。
- 保留既有 tools crate tests，覆盖 execution gate/error metadata、tool catalog snapshot drift、session registrations、asset lease、MCP integration、Skill load、HTTP/network policy；授权决定仍由现有 `AuthorizationEngine` 负向测试和 Agent tool-runner receipt tests 覆盖。
- 验收命令：`cargo fmt --all`、`cargo test --locked -p haven-tools`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`git diff --cached --check`。

## 回滚

删除 `authorization_policy.rs` 与相关 regression test，将授权请求准备恢复到 `AuthorizedExecutor`，并还原 `ToolCatalogSnapshot` 的本地 fallback；撤回本 ADR、索引、架构与路线图记录。无数据库、IPC、配置或用户数据迁移。
