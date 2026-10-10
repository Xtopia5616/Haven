# 0893：通过授权 port 隐藏 AuthorizationEngine

## 状态

已接受并实现（2026-10-10）。

## 背景

`AuthorizationEngine` 是 `haven-tools` 内部唯一的授权决策与进程内 grant owner，但 `ToolServices` 曾把 `Arc<AuthorizationEngine>` 直接交给 App 和 Agent。`SessionToolPorts` 也把具体引擎存进 `SessionSupervisor`，让 Agent 可直接调用授权实现。已有的 `ToolAuthorizationPort` 只负责构造授权请求，决策、回执校验、grant 恢复和清理仍绕过该 port。

这使授权的核心能力依赖具体结构；即使外层有 `ToolsFacade`，调用方仍能直接持有、调用或替换实现。授权涉及安全策略，编译边界应阻止 Agent/App 命名并依赖引擎类型。

## 决定

- `haven-tools` 定义 `AuthorizationPort`，覆盖 typed decision、receipt verification、grant 与 trust 管理、永久权限管理、策略摘要和受控策略更新。
- `AuthorizationEngine` 只在 `haven-tools` 内部构造，并实现 `AuthorizationPort`；默认生产 crate API 不再导出具体引擎。
- `ToolServices.authorization` 只暴露 `Arc<dyn AuthorizationPort>`。
- Agent 的 `ToolAuthorizationPort` 同时拥有请求构造与授权能力；`SessionSupervisor` 和 `SessionToolPorts` 只接收该 Agent port，不再保存具体 `AuthorizationEngine`。
- App composition adapter 负责把 Tools 的授权 port 映射为 Agent 的消费端 port。App 命令通过 `AuthorizationPort` 执行授权与权限管理。

## 影响

- 默认构建的下游 crate 无法导入或构造 `AuthorizationEngine`，只能获得领域 port。
- Agent 与 App 的授权调用、确认回执、策略决策、grant 持久化顺序和清理语义不变；没有新增授权来源或第二份策略状态。
- `ToolServices` 中 MCP、Skills、资产、ToolRun 与 live-output 等其它服务字段仍保留现状；它们是否也应由窄 port 替换，继续按 §5.7 的真实消费者和 owner 证据逐项审查。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- 只把 `AuthorizationEngine` 改名或藏在 `ToolServices` 的私有 accessor 后：拒绝，因为类型和调用能力仍由下游绑定，无法替换实现或由编译器确认依赖边界。
- 仅保留已有请求准备 port：拒绝，因为执行前决策、回执验证与 grant 生命周期仍会绕过该边界。
