# ADR 1277：有界关闭 MCP stdio 进程代次

## 状态

已实现。该 ADR 只收敛 MCP 进程代次关闭顺序，不接通 brokered launcher；受管 package 在 host spawn 前仍被拒绝。

## 背景

MCP stdio transport 已把进程生命周期放在 `McpStdioProcess` port 后（ADR 1270）。但 `shutdown_inner` 先取消了同一个 token，reader task 随即退出，随后 legacy `shutdown` request 无法读取响应；关闭路径再直接等待 `process.wait()`，没有等待上限。对未来 broker generation 来说，无界 wait 会让配置撤销、策略更新和 manager shutdown 卡住。

## 决定

- 正常请求取消与 stdout reader 生命周期分开。取消用户请求后，reader 继续读取已启动进程的有限行协议流，直到 EOF 或 `StdioInner` 被释放；因此 legacy shutdown response 仍由唯一的 MCP JSON-RPC reader 处理。
- Legacy generation 使用共享 2 秒 deadline 发送 `shutdown` request 并等待 response，再发送 `exit` notification。即使 shutdown request 失败，只要 deadline 尚有余量仍尝试发送 `exit`。
- 所有 stdio generation 随后有界关闭 stdin：writer lock 和 `AsyncWrite::shutdown` 各最多等待 500ms，writer 无论成功与否都会被释放，使 server 收到 EOF。
- 关闭 stdin 后最多等待 2 秒自然退出。超时后调用非等待式 `start_kill`，再最多等待 2 秒确认退出。wait future 必须可取消并可重新创建；`start_kill` 必须本身有界且不等待进程退出。
- 超时、关闭或强制回收失败会记录经过清洗的诊断并继续清空 client generation；`StdioInner::drop` 保留最后一次 best-effort termination。日志不记录命令、参数、环境或路径。
- 适用于本机与将来的 brokered stdio process port；不改变 MCP framing、工具授权、network policy、配置或持久数据。HCS guest 的独立停止确认、server-generation revoke 和 broker session 并发/通道协议仍是后续实现。

## 替代方案

- 取消请求 token 时同时取消 stdout reader：拒绝，会丢失 legacy shutdown response，并破坏 transport 唯一 reader 对 pending request 的关联。
- 仅发送 shutdown/exit 后无限等待：拒绝，server 或 broker wait 实现可能使授权撤销和应用关闭永久停滞。
- 超时后只丢弃 process handle：拒绝，无法保证 server generation 的进程树收到终止请求。

## 影响与回滚

改动限于 `haven-mcp` 内部 transport lifecycle 和 `McpStdioProcess` port 的实现要求。没有数据库、配置或 IPC 持久契约变化，无需重置数据。回滚时恢复旧 shutdown 路径，但 broker launcher 接入前必须先恢复有界回收语义。

## 验证

- 回归测试证明 normal request token 已取消时，legacy response reader 仍接收 `shutdown` response，随后写入 `exit` 并将 stdin EOF 交给 server。
- 生命周期测试证明 wait 可在 deadline 返回，再在 kill 后重新等待并观察退出。
- 通过 `cargo fmt --all -- --check`、`cargo test --locked -p haven-mcp` 和 `cargo clippy --locked -p haven-mcp -- -D warnings`。
- 本 ADR 不启动 HCS service、guest MCP server 或运行期配额验收。
