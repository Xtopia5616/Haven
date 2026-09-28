# ADR 0388：Agent Tools ports 与组合层适配器完整收口

- 状态：已采纳并实现（2026-09-28）
- 关联：[ADR 0224](0224-tool-catalog-port.md)、[ADR 0225](0225-tool-observation-port.md)、[ADR 0237](0237-scheduled-authorization-port.md)、[ADR 0374](0374-typed-session-cleanup-and-explicit-agent-tool-wiring.md)、[ADR 0384](0384-agent-tools-execution-context.md)

## 背景

ADR 0374 有意保留 Agent 执行 runner 和 prompt/catalog/observation adapters 对
`ToolsManager` 的依赖，作为阶段 4 后续边界。ADR 0384 加入了 `ToolExecutionContext`
和 ports bundle，但 manager-backed adapters 仍定义在 `haven-agent`，执行/授权准备也
共用同一个 execution port，因此阶段 4 的“Agent 只依赖能力接口；manager adapters 只在
组合边界创建”验收尚未完成。

## 决定

1. `haven-agent` 对外提供可组合的 `PromptToolPort`、`ToolCatalogPort`、
   `ToolExecutionPort`、`ToolAuthorizationPort`、`ToolObservationPort`、
   `SessionToolOverlayPort` 和 `ManagedAssetLeasePort`。`AgentToolPorts::new` 与
   `SessionToolPorts::new` 只接收这些 ports 及既有 typed `AuthorizationEngine`、
   `ActionService` capability；生产构造不接受 `ToolsManager`。
2. 一次执行继续由 `ToolExecutionContext` 传递可选 `session_id`、工具名、输入、取消 token
   和稳定 `step_id`。`ToolExecutionPort` 只执行调用并读取该结果所需的 registrations；风险
   和 `AuthorizationRequest` 准备由独立 `ToolAuthorizationPort` 提供。授权决定、receipt 验证
   与交互确认仍由同一 live `AuthorizationEngine` 持有，并在执行前完成。
3. `haven-app-binary` 在 composition root 创建唯一的 manager-backed adapter，并以同一个
   adapter 实例填充 prompt、catalog、authorization、execution、observation、overlay 和 asset
   ports；`ToolServices.authorization` 与 `ToolServices.actions` 的现有共享实例原样注入。
   session runner、ReAct engine 和 prompt builder 不持有或调用 `ToolsManager`。
4. Agent 内为既有真实工具集成测试保留的 manager fixtures 全部限于 `cfg(test)`；生产 adapter
   的映射回归放在 `haven-app-binary`。
5. 工具 catalog snapshot、live authorization、执行前确认、session/step 身份注入、取消、
   observation 截断、prompt eager-registry fallback、overlay 和 asset lease 的 owner 与顺序不变。

## 替代方案

- 继续在 `haven-agent` 内定义 manager adapter，再由 app 传入 adapter bundle：保留了生产
  Agent crate 对具体 manager facade 的依赖，与阶段 4 的组合边界验收不符，拒绝。
- 把授权决策或 mutable catalog/asset 状态复制到 Agent：会形成重复 owner，并破坏 live grant
  撤销、snapshot generation 和 session lease 的既有语义，拒绝。
- 将所有工具内部 service 都抽成独立 ports：增加重复接口和装配点；本决定仅暴露 Agent 跨 crate
  实际使用且需要替换测试的 capability，拒绝。

## 影响与验证

本 ADR 只调整 Rust 内部构造和依赖方向，不改 tools/provider wire、确认流程、持久化 schema、
IPC、用户数据或 timeout/cancellation policy。Agent production runtime 不再依赖具体
`ToolsManager`；app composition adapter 继续调用原 façade，保持工具行为和共享 runtime owner。

验证命令：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-agent
cargo test --locked -p haven-app-binary
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

## 回滚

将 `haven-app-binary` manager adapter 移回 Agent-owned adapters，恢复 test fixture factory 为
生产构造入口，并撤回本 ADR 与路线图/架构记录。无数据库、配置、IPC 或用户数据重置。
