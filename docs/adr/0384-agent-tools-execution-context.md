# ADR 0384：Agent Tools 执行上下文与端口 bundle

- 状态：已采纳并实现（2026-09-28）
- 关联：[ADR 0257](0257-agent-layer-injects-tool-catalog-port.md)、[ADR 0345](0345-tools-manager-facade-audit.md)、[ADR 0374](0374-typed-session-cleanup-and-explicit-agent-tool-wiring.md)

## 背景

ADR 0374 删除了 Agent 的 service locator，并让组合根显式注入共享 `ToolsManager`，但 `SessionSupervisor` 的执行路径以及 prompt/catalog/observation 仍直接调用 manager facade。Agent runtime owners 因此还同时理解执行参数和多个 Manager 接口，目录展示也与执行输入没有明确的类型边界。

## 决定

1. 在 `haven-agent` 组合边界增加 `AgentToolPorts`。App composition root 从唯一共享的 `ToolsManager` 创建 prompt、catalog 和 session capability adapters；`AgentLayer::build` 与生产 `SessionSupervisor::new` 分别只接收 bundle 中所需端口。`SessionSupervisor` 不再保存生产 `ToolsManager` 或 manager-backed catalog 字段。
2. 每次工具执行由 `ToolExecutionContext` 表达，包含可选 `session_id`、`tool_name`、JSON `input`、`CancellationToken` 和稳定的 `step_id`。Manager adapter 将这些值原样转交到既有 `execute_tool_with_step`；step metadata registrations 也通过执行 port 查询。
3. 确认仍在工具执行前由调用方完成。Authorization request 与风险在执行时从 live policy 读取；只有 action-step 显示 metadata 从该 turn 已捕获的 immutable `ToolCatalogSnapshot` 解析，执行不能换到较新的目录 generation。
4. Prompt 只依赖只读 `PromptToolPort`，一次读取目录版本/内容及运行时 prompt context；内置工具初始化尚未完成时继续使用 eager registry fallback。catalog、session overlay、observation 和 managed-asset lease 分别使用窄 adapter。
5. `ToolsManager` 作为组合 facade 仍存在于 adapter 实现中；本 ADR 的边界是禁止其穿透 Agent runtime owners，不把每个 Tools 内部 service 再拆成重复状态 owner。ADR 0374 已明确接收的 authorization/action capability 保持其现有 owner 和执行语义。

## 影响与验证

不修改 tool/provider wire、确认流程、持久化 schema、用户数据或 cancellation/deadline policy。prompt catalog version 与 fallback、工具确认门禁、session overlay 生命周期和 managed-asset lease 行为保持不变。新增回归覆盖 adapter context 转发与目录契约；阶段验收运行 Agent/Tools 测试及 workspace 检查。

```text
cargo test --locked -p haven-agent
cargo test --locked -p haven-tools
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
```

## 回滚

恢复 AgentLayer 接收 manager 与 supervisor manager-backed execution wiring，并同步撤回 `AgentToolPorts`、窄端口及其调用方即可；不需要数据库、配置或 IPC 重置。
