# ADR 1113：统一 MCP 连接入口的并发上限

## 状态

已接受（2026-10-13）

## 背景

MCP reload 在 AdminServices 中最多并发启动 8 个连接，但 manager 启动配置时会为每个 server 同时创建任务，动态连接和客户端 health monitor 的自动重连也不受该局部 semaphore 控制。大型配置或多个并发调用因此可能同时启动大量外部进程或网络请求，且不同入口的资源预算不一致。

## 决定

- `haven_mcp::MCP_MAX_PARALLEL_CONNECTIONS` 是唯一连接尝试并发上限，当前为 8；`McpManager` 的 clone 共享同一个 semaphore。
- 配置加载在创建客户端和 notification listener 前取得 permit，并限制 JoinSet 中未收割的连接任务不超过 8。连接完成、失败或任务取消时由 RAII 释放 permit。
- 动态连接、手动 reconnect 与 health monitor 自动重连取得相同 permit；Admin MCP reload 使用同一公开常量限制其调度队列。所有 server connection entry point 因而共用同一活动连接上限。
- 该决策限制并发尝试数，不限制配置中的 server 总数，也不改变失败重试、network policy 或 durable 配置语义。

## 替代方案

仅保留 `mcp_reload` 局部 semaphore 不能覆盖启动、普通动态连接和自动重连；在配置校验中限制 server 总数改变了用户可配置的数量，且无法限制多个独立入口同时启动。

## 影响

超过 8 个并发连接需求时，后续入口等待共享 permit；已连接 server 不占连接名额。配置加载最多保留 8 个未收割的连接任务，避免大配置创建同规模后台 JoinHandle。上限是进程内资源预算，不是协议或 IPC 契约。

## 验证

- `cargo test --locked -p haven-mcp manager_lifecycle_tests`
- `cargo test --locked -p haven-mcp mcp_monitor_cancels_during_reconnect_backoff`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- UI、IPC contract 与 ADR index 检查。

## 回滚

不得只恢复 reload 局部 semaphore。若调整连接预算，manager 所有初次连接、动态连接与自动重连必须继续共享一个上限，并且批量加载要同时限制活动任务与未收割 JoinHandle。
