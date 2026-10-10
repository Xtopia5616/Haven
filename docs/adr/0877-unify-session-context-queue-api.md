# ADR 0877：统一 Session context 队列出入口

## 状态

Accepted — 2026-10-10

## 背景

Session 队列原有 `add_tool_run_completion` convenience 方法和 `add_tool_run_completion_with_id`，最终都调用 Actor 的 `add_tool_run_completion`。生产交付路径使用带稳定 result ID 的版本。Actor 的 `BackgroundResult` 命令也只描述 background 来源，但相同队列还会接收 scheduled ToolRun 的终态结果。

入队侧之外还存在 `drain_follow_ups`、`drain_steering`、`drain_tool_run_completions` 三种单队列读取方法与对应 Actor mailbox command。它们只被测试调用，绕过生产 ReAct 路径的排序和批次预算；ToolRun 专用 façade 还把 `ToolRunResult` 投影成 `Vec<String>`，丢弃跨 broadcast 重投和 transcript retry 所依赖的 `tool_run_result_id`。

## 决定

- 将两层入队方法和纯队列 helper 统一为 `enqueue_tool_run_result`，mailbox 命令命名为 `EnqueueToolRunResult`。唯一 API 必须携带稳定的 `tool_run_result_id`，同时覆盖 background 与 scheduled 来源。
- ReAct context 统一从 `drain_react_context` 出队；Actor 层同样采用该名称与 mailbox command `DrainReActContext`。
- 删除 `drain_follow_ups`、`drain_steering` 和 `drain_tool_run_completions` façade、Actor 方法及 mailbox command；`drain_react_context` 成为唯一待处理 context 出队协议，执行统一优先级和预算。ToolRun result 的 ID 与正文由 `ReActContextBatch` 一并返回。
- 保留各队列在 Actor 中的容量限制、优先级、取消和 transcript 投影行为。

## 影响与兼容性

本次为 Agent 内部 API 清理，无 IPC、配置或持久化变化，无需重置；不保留旧入口 alias。测试中的调用改为唯一批量出队协议，并直接核对稳定 result ID。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`，以及新 ADR 文件的 Prettier 检查。
测试套件未运行。

## 回滚

不恢复正文-only dequeue。若出现独立消费场景，应设计保留 result ID 且明确状态 owner 的专用批次，而不是复制队列 drain command。
