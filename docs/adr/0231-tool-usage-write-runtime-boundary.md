# ADR 0231：工具 Usage 批量写入归属 UsageRuntime

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` ReAct 工具 Usage 持久化路径
- 关联：[ADR 0218](0218-usage-write-boundary.md)、[ADR 0223](0223-usage-runtime-boundary.md)

## 背景

`ReActEngine` 仍持有一份 `Arc<Database>`，但在该路径中只用于工具 Usage 批次的阻塞写入；真正的 durable 写入已经由 `SessionStore::append_usage_batch` 完成。`UsageRuntime` 同时拥有该 store 和 Database blocking executor，继续让 ReActEngine 直持有这份依赖会重复表达 Usage 写入边界。

## 决定

1. `UsageRuntime::append_tool_usage_batch` 接收 session、`LlmCallUsageInput` 批次和可选取消令牌。
2. ReActEngine 的 `record_tool_usage` 只调用该窄方法；批次不进入累计 Usage 的 per-session FIFO，保持原来的批次事务和写入排序。
3. `run_blocking`/`run_blocking_cancellable`、`SessionStore::append_usage_batch`、`usage_recorded` 事件和 UI Usage 通知语义保持不变。
4. ReActEngine 仍可因 ContextSource、transcript 等独立生产路径保留 Database 依赖；本 ADR 不声称已完成全局 Database facade 收口。

## 替代方案

- 把工具 Usage 批次并入累计 Usage FIFO：会改变现有排序与生命周期，拒绝。
- 恢复 raw Database 写入：会绕过既定 SessionStore 边界，拒绝。
- 现在删除 ReActEngine 的全部 Database 字段：仍有 transcript/event boundary 等独立调用者，超出本切片范围。

## 影响与验证

Usage 写入的所有权更集中，ReActEngine 不再为工具 Usage 保存重复持久化入口；schema、事件格式和 IPC 不变。验证包括 Agent 全量测试、取消测试重复运行、workspace check、Agent clippy 和格式检查。

## 回滚

恢复 ReActEngine 的工具 Usage blocking closure，并删除 `UsageRuntime::append_tool_usage_batch` 及本 ADR；无需数据重置。
