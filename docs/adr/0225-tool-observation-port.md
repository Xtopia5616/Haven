# ADR 0225：Agent ToolObservationPort 观察文本边界

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 对已完成工具结果的观察文本读取
- 关联：[ADR 0224](0224-tool-catalog-port.md)、[ADR 0163](0163-typed-authorization-engine.md)

## 背景

`SessionSupervisor` 既向 ReAct 提供观察文本，也在工具完成后为步骤持久化读取观察文本。两处都直接依赖 `ToolsManager`，使 Agent 的只读观察职责与工具执行 facade 耦合。

## 决定

1. `haven-agent` 定义 async `ToolObservationPort`，唯一 API 为
   `observation_text(tool_name, result) -> String`。`SessionSupervisor` 构造时创建并持有生产 adapter；adapter 捕获 supervisor 使用的同一个 `Arc<ToolsManager>`，并委托现有 `ToolsManager::observation_text`。
2. ReAct 经 `SessionSupervisor::observation_text` 读取观察文本；工具执行后的步骤持久化观察也经该 port 读取。两处仍独立调用并保留各自现有时序，不合并或缓存结果。
3. 此 port 只格式化已产生结果的观察文本。它不执行工具、不评估风险、不创建授权请求、不确认授权，也不验证或签发授权 receipt。`execute_gated`、授权与 receipt 流程、`ToolsManager` 执行入口及输出格式/上限均保持原样。
4. 后续若引入 `ExecutionPort` 或 `AuthorizationPort`，必须继续使用同一个 live `AuthorizationEngine` 实例，即 `ToolServices.authorization` 所持的共享 `Arc`。不能为新 port 创建独立 engine、授权副本或静态决策快照，以保留即时撤销与运行时策略一致性。

## 替代方案

- 继续让两个调用点直接依赖 `ToolsManager`：观察读取边界会继续散落在执行 facade 依赖中。
- 把执行或授权也一并抽取：扩大本切片的安全行为面；观察文本格式化不需要拥有这些能力。
- 合并两处观察读取：会改变当前读取时序与步骤持久化路径的独立性。

## 影响

Agent 对观察格式化的依赖收窄为一个只读 port。生产 adapter 复用同一个 `ToolsManager`，所以沿用现有工具级/全局输出上限和结果文本，不引入新格式或策略。

## 验证

- adapter 单测通过工具专属输出上限验证工具名和结果委托给现有 formatter。
- `cargo fmt -p haven-agent -- --check`
- `cargo check --locked -p haven-agent`
- `cargo test --locked -p haven-agent`
- `cargo clippy --locked -p haven-agent -- -D warnings`
- `git diff --check`

## 回滚与重置

无持久化或配置变化，不需要数据重置。回滚时恢复两个调用点直接调用 `ToolsManager::observation_text`，并移除该 port、adapter 及本 ADR。
