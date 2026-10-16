# ADR 1270：抽取 MCP stdio 进程生命周期端口

## 状态

已实现。该 ADR 只提供 transport/process launcher 边界；当前默认 launcher 仍在本机启动进程，`WorkspaceWrite` 下 stdio MCP 继续 fail closed。

## 背景

ADR 1269 规定 brokered MCP 必须保持 `haven-mcp` 为 JSON-RPC framing、协议版本、请求关联、通知和取消的唯一 owner。下一步需要把长生命周期 server 的进程创建/生命周期从协议 transport 中分离，使 HCS broker 可以提供双向流，而无需在 broker 中重写 MCP protocol client。

现有 stdio child 将 `stderr` 配置为 pipe，但没有消费者。server 大量写日志时，pipe 缓冲区会填满，进而阻塞 server 的协议处理。MCP stdio 将 `stderr` 定义为可选诊断输出，不参与 JSON-RPC framing，因此客户端必须持续消费它。

## 决定

- `haven-mcp` 定义 `McpStdioProcess` 和 `McpStdioProcessLauncher` 端口。每个 MCP client generation 继续拥有一条双向协议流和对应进程生命周期；local launcher 保留现有 Windows process containment 行为。
- manager 对新 generation 使用当前 launcher。替换 launcher 会断开所有活跃 stdio generation，避免旧 generation 在 reconnect 时沿用已撤换的执行边界。HTTP transport 不受此设置影响。
- `McpStdioLaunchSpec` 是配置快照，Debug 输出只显示 server 名称、参数数量和配置字段是否存在，不输出 command、参数、环境值或工作目录内容。broker launcher 必须把 server 映射到受信任的 runtime/bundle，不得把配置中的宿主 command、cwd 或绝对路径当成 guest 命令执行。
- launcher 必须持有一次性 process-start permit，直到本地进程已附加 OS containment，或 broker 确认 process generation 已启动。MCP protocol owner 只接收可靠的 stdin/stdout 等价字节流和 process lifecycle 操作。
- `stderr` 是可选流，由独立 task 持续复制到 sink，以固定缓冲内存消费；不缓存、解析或转发日志，也不影响 MCP 请求结果。stream 初始化失败时，transport 立即请求终止已经启动的 process。
- 当前仍使用 local launcher。此切片不改变 MCP 配置格式、暴露策略、权限决策、WorkspaceWrite 行为、HCS broker protocol 或 workspace review；`WorkspaceWrite` 下 stdio server 继续在 spawn 前拒绝。实际 HCS launcher、受限 runtime/bundle profile、网络 gate 和 generation 撤销仍是后续切片。

## 替代方案

- 在 HCS broker 中重新实现 JSON-RPC client：拒绝，会产生第二个协议 owner，并复制 framing、版本协商、关联和取消逻辑。
- 缓冲或保留完整 `stderr`：拒绝，日志长度不受 server 信任，可能造成内存增长；当前没有稳定的诊断消费契约。
- 在 `WorkspaceWrite` 下回退到本机 launcher：拒绝，会越过 ADR 1269 的执行边界。默认 local launcher 只维持既有授权路径，不为受限模式开放执行。

## 影响与回滚

`haven-mcp` 内部 stdio transport 现在依赖进程端口；`McpManager` 可为未来 broker 接线指定 launcher。默认行为和配置持久化不变。若后续 HCS 接线遇到问题，可移除 launcher 注入并恢复 transport 直接持有本地 Child；stderr drain 则应保留，因为不依赖 broker。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-mcp`：103 项通过。
- `cargo test --workspace --locked`：全 workspace 通过；包含 Agent、Memory、MCP、Tools 等 crate 测试。
- 注入 launcher 测试验证 MCP request/response 仍由同一 protocol owner 处理、launch spec Debug 脱敏且 generation drop 会终止进程。
- stderr 测试向 16-byte duplex pipe 写入 1 MiB 数据，验证有界 drain 能保持 writer 前进。
- 本 ADR 不代表已启动 HCS broker、guest MCP server 或完成 guest 写入配额验收。
