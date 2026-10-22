# ADR 1270：抽取 MCP stdio 进程生命周期端口

## 状态

已实现。MCP 管理器使用本机 launcher；`WorkspaceWrite` 下 stdio MCP 在进程启动前拒绝。

## 背景

`haven-mcp` 负责 JSON-RPC framing、协议版本、请求关联、通知和取消。stdio transport 将这些协议操作与子进程的 stdin/stdout/stderr、退出和清理分开管理。

现有 stdio child 将 `stderr` 配置为 pipe，但没有消费者。server 大量写日志时，pipe 缓冲区会填满，进而阻塞 server 的协议处理。MCP stdio 将 `stderr` 定义为可选诊断输出，不参与 JSON-RPC framing，因此客户端必须持续消费它。

## 决定

- `haven-mcp` 使用内部 `McpStdioProcess` 与 `McpStdioProcessLauncher` 接口分开管理 JSON-RPC 流和宿主进程生命周期。每个 MCP client generation 仍拥有一条双向协议流及其子进程；生产实现固定使用本机 launcher，并保留现有 Windows process containment 行为。
- 启动配置的 Debug 输出只显示 server 名称、参数数量和配置字段是否存在，不输出 command、参数、环境值或工作目录内容。
- 子进程必须持有一次性 process-start permit，直到本机进程已加入 OS containment。MCP protocol owner 只接收 stdin/stdout 字节流和进程生命周期操作。
- `stderr` 是可选流，由独立 task 持续复制到 sink，以固定缓冲内存消费；不缓存、解析或转发日志，也不影响 MCP 请求结果。stream 初始化失败时，transport 立即请求终止已经启动的 process。
- 本机 launcher 是唯一生产实现。`WorkspaceWrite` 下 stdio server 在 spawn 前拒绝；HTTP transport 不受此限制。

## 替代方案

- 缓冲或保留完整 `stderr`：拒绝，日志长度不受 server 信任，可能造成内存增长；当前没有稳定的诊断消费契约。
- 在 `WorkspaceWrite` 下回退到本机 launcher：拒绝，会绕过进程启动前的策略拒绝；当前进程边界见 [ADR 1262](1262-separate-process-and-network-sandbox-boundaries.md)。

## 影响与回滚

stdio transport 通过本地进程 owner 管理 generation。stderr drain 使用固定缓冲持续消费，不缓存日志；配置和数据库格式不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-mcp`：103 项通过。
- `cargo test --workspace --locked`：全 workspace 通过；包含 Agent、Memory、MCP、Tools 等 crate 测试。
- 注入 launcher 测试验证 MCP request/response 仍由同一 protocol owner 处理、launch spec Debug 脱敏且 generation drop 会终止进程。
- stderr 测试向 16-byte duplex pipe 写入 1 MiB 数据，验证有界 drain 能保持 writer 前进。
