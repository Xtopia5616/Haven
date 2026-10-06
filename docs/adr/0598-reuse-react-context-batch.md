# ADR 0598：SessionActor 复用 ReactContextBatch

## 状态

已采纳并实施。

## 背景

`SessionActor::drain_context` 从 actor mailbox 返回三组数据：steering、follow-up 和 ToolRun completion results。它原以三元素 tuple 表达；唯一的 `SessionSupervisor::drain_react_context` 调用点随即把 tuple 包装为现有的 `ReactContextBatch`，actor 命令失败时也需手工构造三个空 vec。队列投影已有稳定的具名类型，但 mailbox 边界没有复用。

## 决定

1. Actor 的 `DrainContext` mailbox reply 与 `SessionActor::drain_context` 直接使用 `ReactContextBatch`。
2. 将 `ReactContextBatch` 定义移到 actor 模块，因为它现在就是 actor drain 命令的响应投影；Session module 仍向 ReAct context 消费者重导出它。
3. supervisor 直接返回 actor 的具名批次；失败时继续返回 `Default` 空批次。

## 替代方案

- 保留 tuple 并只在 supervisor 处包装：拒绝，actor mailbox 消费者仍需位置解读，且重复构造批次。
- 在 actor 模块另建一个内容相同的结果类型：拒绝，会制造 mailbox 与 ReAct 输入的重复 owner。

## 影响与验证

- 仅更改 Agent 进程内 mailbox 响应类型，不改变队列预算、steering 优先级、抽取顺序、请求投影或持久化/IPC 行为。
- 命名路线图仍保持 Active；Session interaction gates 和其余 crate/UI/IPC 域继续审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings`、`cargo test --locked -p haven-agent`、ADR 索引及 staged diff 检查。

## 回滚

恢复 actor mailbox 的 tuple 响应和 supervisor 二次构造；无持久化迁移。
