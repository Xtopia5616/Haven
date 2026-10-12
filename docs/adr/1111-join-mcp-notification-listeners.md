# ADR 1111：由 MCP 生命周期 owner join notification listeners

## 状态

已接受（2026-10-13）

## 背景

MCP manager 启动的 `tools/list_changed` consumer 与 HTTP SSE producer 都会继续运行，但原有调用路径丢弃 JoinHandle。连接失败时 consumer 可继续持有 client 并等待通知；HTTP SSE 在 `tools/list` 失败后也可能在 transport 未关闭时重试。shutdown/reconnect 取消 token 却没有等待 listener 离开，生命周期边界因此无法确认。

## 决定

- `McpManager` 按 client 实例保留 catalog notification consumer；remove、失败连接清理和 `shutdown_all` 先关闭 client，再取出并 join consumer。listener map 锁不跨 await。
- `McpClient` 按连接代次持有 HTTP SSE producer JoinHandle。transport token 取消后，shutdown/reconnect join 当前 producer；超过 5 秒时记录 warning 并 abort。
- transport 请求取消只代表当前连接代次结束，manager-owned consumer 留待重连后的新代次；终态 `client.shutdown()` 通过独立 lifetime token 停止 consumer。
- `connect()` 失败时清理已安装的 client transport；动态 `connect_server()` 失败还取消并 join其 manager-owned consumer。
- 新增覆盖失败动态连接、加载后 shutdown、client SSE handle join 的测试。

## 替代方案

只取消 token 而不 await，无法证明 listener 已退出；把 consumer handle 放进 client 又会令任务持有的 `Arc<McpClient>` 与 client 内的 JoinHandle 形成自持有生命周期。Manager 按实例持有 consumer，client 仅持有不捕获 client 的 SSE producer，避免此环。

## 影响

连接/关闭顺序增加 listener join barrier；正常工具行为与 MCP wire shape 不变。超过 5 秒未停止的 listener 被显式 abort 并记录；SSE producer 只会在现有连接代次取消后收尾。

## 验证

- `cargo test --locked -p haven-mcp`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- IPC 与 ADR index 检查。

## 回滚

不得恢复丢弃 listener JoinHandle 的行为。任何替代 owner 都必须在 transport/client 关闭后 join SSE producer，并在 client 终态 shutdown 后 join manager notification consumer。
