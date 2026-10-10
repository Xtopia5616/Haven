# ADR 0828：并行执行有界的 MCP 重载并保留未知结果

## 状态

Accepted — 2026-10-09

## 背景

`mcp_reload` 先移除所有 live clients，再顺序连接每个 enabled server；单个工具调用只有 10 秒总预算。多台 npx/外部 server 启动延迟会累加，造成操作超时或长时间无反馈。超时发生时，部分 server 可能已经重新连接，盲目重放也会再次断开现有 session。

## 决定

- MCP reload 对所有 enabled servers 并行连接，最多同时启动 8 个连接；每个 server 仍单独返回 success/error 行，最终输出按 server name 稳定排序。
- MCP reload 工具预算提高到 60 秒。MCP admin 工具组将超时结果分类为 `TimedOutUnknown`；`mcp_reload` 的 idempotency 降为 `Unknown`，因为超时/取消可发生在部分 server 已完成连接之后。
- 未引入异步 job/progress 查询。超过 60 秒仍会返回未知结果，调用方必须先查 `mcp_list` 再决定是否重试。

## 替代方案

- 继续串行连接并提高固定预算：拒绝。服务数增加时连接延迟仍会累加。
- 无上限并行连接全部 server：拒绝。大配置会同时创建过多外部进程和连接。
- 超时后一律声称已终止并重试：拒绝。连接任务可能已部分完成，重放会再次重置状态。

## 影响与验证

`McpReloadOutput` shape 不变，连接完成顺序不影响输出顺序。没有配置或持久数据变化。适用门禁：Rust workspace 编译、严格 Clippy、格式与 diff 检查。测试因本次执行约束未运行；超出 60 秒的 reload progress 仍无查询接口。

## 回滚

恢复串行连接、10 秒预算和 Idempotent/TIMED_OUT_TERMINATED 元数据会重新引入长配置下超时与不安全重试风险。仅改变进程内连接编排和工具恢复分类；无需配置或数据库重置。
