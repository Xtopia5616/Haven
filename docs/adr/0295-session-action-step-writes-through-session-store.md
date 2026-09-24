# ADR 0295：SessionSupervisor action-step 写入通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：`SessionSupervisor` / `tool_runner` 的 action-step durable 写入调度
- 关联：[ADR 0157](0157-session-supervisor-actor-run-engine.md)、[ADR 0196](0196-session-actor-event-sourced-state.md)、[ADR 0258](0258-failed-action-step-cleanup-session-store-port.md)、[ADR 0294](0294-session-interaction-events-through-session-store.md)

## 背景

ADR 0294 将 interaction domain event 读写移到 `SessionStore` 后，`SessionSupervisor` 仍保留 `Arc<Database>` 字段供 `tool_runner` 写 action-step。pending ensure、ensure 后 start、ensure 后 finish 都由 Agent 直接调度 blocking pool；`execute_step` 还将 ensure 与 finish 放在同一个 closure 中，以免扩大两次数据库操作之间的并发窗口。

Action-step 风险、静默属性、确认结果、工具执行与 outcome 决策属于 Agent。Memory 只需接收稳定的 owned 持久化输入，并在 blocking pool 上复用既有 Database 操作。

## 决策

1. 在 session-steps domain 定义 `ActionStepWrite`，owned 字段包含 session/step identity、tool invocation identity 和已决 metadata。确认状态作为端口参数传递；Memory 不依赖 `ActionStepContext` 或其他 Agent 类型。
2. `SessionStore` 提供 pending ensure、ensure-and-start、ensure-and-finish 三个异步窄端口。两个复合端口各自在一个 `run_blocking` closure 内按原顺序调用 `Database::ensure_action_step_with_identity` 后调用既有 start/finish 操作；返回 bool、错误、幂等、状态转换、确认更新和 `step_id` 行为不变。
3. `ActionStepContext` 只在 Agent 内构造 typed write input 与 metadata。`begin_action_step`、start、finish 和 `execute_step` 经由 supervisor 已有的 `SessionStore` 字段调用端口。begin/finish 日志、错误文案与 `ActionStepPersistenceError` 映射保持不变。
4. 删除 `SessionSupervisor` 的 raw `Database` 字段；构造入口仍以共享 Database 创建该 supervisor 自己的 `SessionStore`。action-step policy、metadata、授权及终态选择继续由 Agent 拥有。此切片不改变 ActionService 或 session-events transcript 写入。

将 action-step 决策下沉至 Memory 会反转职责；把 ensure/start 或 ensure/finish 拆成两个异步调用会扩大原有并发窗口；保留 supervisor raw 字段则继续暴露 blocking 调度边界。

## 影响与验证

- 无 schema、IPC、event payload 或用户数据契约变化，无需数据库重置。
- Memory 测试覆盖 identity、confirmed、start 幂等、observation、outcome 和 terminal row 行为；Agent 测试覆盖 tool-runner 经 SessionStore 执行 action-step 生命周期。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo clippy --locked -p haven-memory -p haven-agent -- -D warnings`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 回滚

将 tool-runner 的三个端口调用恢复为原有 Database blocking 调度，恢复 `SessionSupervisor` raw Database 字段，并删除新增端口、测试、本 ADR、索引和路线图记录。无需迁移或重置数据库。
