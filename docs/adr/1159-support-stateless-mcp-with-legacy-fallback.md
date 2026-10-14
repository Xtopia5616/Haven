# ADR 1159：支持无状态 MCP 协议并保留旧版回退

## 状态

已采纳并实施（2026-10-14）

## 背景

Haven 的 MCP client 固定请求 `2024-11-05`，所有服务器都先经过 `initialize`，HTTP 请求还会携带 MCP session。`2026-07-28` 移除了 initialize 握手、协议 session 与独立 HTTP GET 通知流；协议版本、客户端身份和能力改为逐请求元数据，Streamable HTTP 还要求协议版本及方法路由头。直接替换旧流程会使现有 stdio 和有状态 HTTP 服务器失联。

## 决定

- 连接时优先探测 `2026-07-28`：stdio 先调用 `server/discover`；HTTP 发送带现代元数据和路由头的 `server/discover` 请求。
- 现代请求都携带协议版本、Haven 客户端信息和客户端能力。HTTP 请求都发送 `MCP-Protocol-Version` 与 `Mcp-Method`；需要路由实体的调用发送经过安全编码的 `Mcp-Name`。现代模式不发送或使用 `Mcp-Session-Id`，也不打开旧版 GET SSE 流。
- 现代 HTTP `tools/call` 支持 `inputSchema` 的 `x-mcp-header` 参数标注；无效定义会从工具目录排除，参数头仅从对应调用参数读取并按规范编码。
- 保留双年代探测：stdio 对非已知现代协议错误回退到 `initialize`，若探测中断了字节流则先重启子进程；HTTP 仅在兼容性相关的 400/404/405 且响应不是已知现代协议错误时回退。已知的现代协议错误不触发旧版尝试。
- `UnsupportedProtocolVersionError` 若列出 Haven 支持的旧版协议，则选择最新的已知旧版本并走 initialize；否则向调用方报告不支持。其余旧版回退继续请求 `2024-11-05`，保留现有初始化与 HTTP session 行为。
- initialize 返回较新的旧版协议时，后续 HTTP 请求在该协议要求时附带 `MCP-Protocol-Version`。只在旧版模式下打开已有 GET SSE listener。
- 不因一次 `tools/call` 失败自动改换协议后重试，避免对非幂等工具重复执行。

## 影响

协议选择只保存在活动 MCP 连接中，不改变配置格式、数据库或 durable 数据，无需重置用户数据。现代 `tools/list` 与 `tools/call` 可无会话工作；已支持的 initialize/session 路径继续可用。身份验证错误、普通 HTTP 服务错误及网络失败不会被误判为协议代际差异。

## 验证

- 覆盖现代逐请求元数据、HTTP 路由头及不安全头值编码、参数头 schema 校验和旧版选择规则。
- 覆盖 HTTP 服务器拒绝现代探测后的 initialize/session 回退。
- `cargo test --locked -p haven-mcp` 与 `cargo clippy --locked -p haven-mcp -- -D warnings`。

## 回滚

此决定没有持久数据或配置迁移。回滚时删除现代探测/逐请求元数据分支并恢复固定旧版 initialize 流程；无需清理任何用户文件。
