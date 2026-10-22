# ADR 1312：拆分 Agent tool runner 私有职责

## 背景

`crates/agent/src/session/tool_runner.rs` 同时实现 session step 持久化、工具执行模式路由，以及 session confirmation 和 scheduled ToolRun 的申请、仲裁、终态投影。它们有不同的失败边界与测试关注点，但都属于当前 `SessionSupervisor` 协调的会话生命周期。工具步骤必须继续复用预先生成的 `step-*` 身份；confirmation 与 scheduled claim 仍须经过 Agent 当前 owner，Tools 继续独占授权策略。

## 决定

1. 将 step identity/persistence 与终态步骤写入迁至私有 `session/tool_runner/step_persistence.rs`；将 foreground/background、Code Mode、workflow 的执行路由迁至 `execution.rs`；将 session 与 scheduled confirmation、expiry、claim 和 terminal transcript 处理迁至 `confirmation.rs`。
2. `tool_runner.rs` 保留原模块入口、稳定 re-export 和现有行为测试入口。`ToolStepMetadata`、`ToolStepPersistenceError` 与确认到期时间 helper 保持原跨模块路径；新模块均为私有。
3. 所有实现继续在同一个 `SessionSupervisor` 上定义 inherent methods，不新增 session/confirmation 状态副本，不改变 actor、Tools authorization engine、持久化契约或事件顺序。
4. step persistence 与 execution 之间仅共享模块内的 request/context、outcome mapper 和持久化 helper；确认测试使用的 helper 只在 `tool_runner` 内可见。测试留在原测试模块，不为文件缩短而搬动。

## 替代方案

- 保留 3,600 行混合模块：拒绝；step projection、执行适配与 scheduled confirmation 有独立调用路径和失败/终态边界，已符合拆私有模块的维护收益条件。
- 将 confirmation 或 ToolRun claim 状态移到第二个 coordinator：拒绝；这会拆散 `SessionSupervisor` 对 session 生命周期的唯一协调职责。
- 合并各执行模式或复制 step metadata：拒绝；foreground/background、Code Mode 与 workflow 需要现有不同的执行 port，但应共用既有 step identity 与 live authorization owner。

## 影响与回滚

本次为模块边界整理，不改变 IPC、数据库 schema、durable event、ID 格式、confirmation 决策、scheduled claim 或用户可见行为，无需重置数据。回滚时将三个私有模块的实现并回 `tool_runner.rs`，保留当前 facade 的调用路径即可。

## 验收

- `cargo test --locked -p haven-agent`：674 passed、1 ignored；doc tests 通过。
- `cargo clippy --locked -p haven-agent -- -D warnings` 通过。
- Windows `cargo test --workspace --locked -- --test-threads=1` 通过；Agent 行为测试通过，Tools 为 976 passed、2 ignored，MCP integration 为 7 passed，workspace doc tests 通过。
- Windows `cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check` 与 `scripts/check-adr-index.ps1` 通过；ADR index 为 1294 项且本地链接均可解析。
- Windows installer/UI 流程仍按路线图 §5.1 作为独立发布 Gate。
