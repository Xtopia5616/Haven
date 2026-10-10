# ADR 0831：将框架级工具失败写入诊断日志

## 状态

Accepted — 2026-10-09

## 背景

`haven.diagnostics.logs_tail` 已读取文件日志，但 skill stdout JSON 解析失败、动态 MCP connect 失败和工具执行超时没有稳定的结构化日志记录，导致诊断工具无法回看这些框架边界错误。`haven.diagnostics.errors` 目前只列出 session store 的错误记录，不能作为通用框架错误源。

## 决定

- skill stdout JSON 解析失败、动态 MCP connect 失败和工具超时通过 `tracing` 写入 error/warn 事件。
- 日志只包含 skill/tool/MCP server 标识、超时与 outcome 或经 `sanitize_error_text` 处理的解析/连接诊断；不记录工具输入、完整 stdout 或命令参数。
- 日志 tail renderer 显示有界日志行、总行数和来源路径；无日志时显示明确空状态，读取失败时显示原因。
- `haven.diagnostics.errors` 继续表示 session 错误列表；框架错误通过 logs_tail 读取，避免两个来源混成一个无 owner 的列表。

## 替代方案

- 把所有 tracing 错误同步复制到 `diagnostics.errors`：拒绝。当前 errors 的 owner 是 SessionStore；添加全局进程内错误 ring 或持久化表需要单独设计容量、生命周期、脱敏和跨重启语义。
- 将 stdout/stderr 全量写入日志：拒绝。skill 输出可能含用户文件内容或凭据，且大小不受日志单行约束。

## 影响与验证

日志事件新增而不改变 ToolResult/IPC 输出、配置或持久化格式。日志文件关闭时 `logs_tail` 继续返回不可用原因，诊断不会伪称已有数据。适用门禁：Rust workspace 编译、严格 Clippy、Rust 格式、UI 类型检查与 diff 检查。测试未运行。

## 回滚

删除上述 tracing 事件与日志专用 renderer 即可；无配置、数据库或缓存重置要求。
