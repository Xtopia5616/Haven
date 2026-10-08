# ADR 0777：Process 与 System renderer 复用闭合输出值

## 状态

已采纳并实施（2026-10-08）。

## 背景

`ProcessTool` 的 operation 来自闭合 `ProcessOperation::{List, Kill}`，但 UI props 和动态 ToolResult guard 曾把它当普通字符串。进程 status 来自 `sysinfo::ProcessStatus`；直接序列化 `Debug` 会使 `Unknown(u32)` 变成无界的 `Unknown(code)` 字符串。`SystemTool` 的输出 scope 则只可能是输入 schema 中的 `info`、`overview`、`env`、`registry`、`power`、`display`、`displays`。UI 没有在 System body 中读取 operation，但此前仍为该未消费字段增加了类型与 shape 检查。

## 决定

1. ProcessTool 显式将 `ProcessStatus` 每个状态映射为稳定字符串；所有 `Unknown(code)` 映射为 `Unknown`。映射使用穷尽 Rust match，sysinfo 新增状态时编译会要求同步审查。
2. UI `ToolProcessOperation`、`ToolProcessStatus` 与 `ToolSystemScope` tuple 分别承载对应 producer 值域；renderer props 和 nested guard 共用这些类型/guard。
3. Process operation/status 或 System scope 的未知动态值回退通用 JSON renderer，并保留原 payload。System operation 不被组件消费，因此从其 props 和 shape guard 移除，畸形额外字段不会降低 renderer。

## 影响与回滚

只改变进程状态的展示投影：未知平台状态码不再附带数值，统一显示为未知；历史 `Unknown(code)` 结果仍可通过通用 JSON 查看。ToolResult envelope、其它进程状态字符串、SystemTool scope、IPC 与持久化不变。若要回滚，恢复原 Debug 格式并移除对应 UI tuple/guard 与 renderer props 类型即可。

## 验收

Rust 单测覆盖未知 code 归一化和具名状态；UI contract test 覆盖未知 operation/status/scope 回退、未知值保留路径及 System 未消费 operation 不触发降级。执行 Rust Tools crate 与 UI 的适用检查、测试和 ADR 索引检查。
