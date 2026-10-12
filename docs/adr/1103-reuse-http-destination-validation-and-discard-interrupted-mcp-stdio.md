# ADR 1103：复用 MCP HTTP 目标校验并丢弃中断的 stdio 连接

## 状态

已接受（2026-10-13）

## 背景

MCP HTTP Ask/Restricted 曾使用本地地址过滤逻辑，与不可信 HTTP 请求的共享目标校验存在策略漂移，包括 IPv4-mapped IPv6 和特殊用途地址。stdio 请求读取若在 JSON-RPC 行中途取消或超时，`BufReader` 可能已经消费了响应前缀；复用同一连接会把后续响应拼接到残余字节上。单纯让下一次 request 失败不能及时通知现有 liveness monitor。

## 决定

- MCP HTTP Ask/Restricted 复用 `haven_common::network::resolve_http_destination`，共享 HTTP(S)、凭据、特殊地址与 DNS 全答案验证，并固定选中的地址；请求关闭代理和自动重定向。Open 仍按用户显式开放策略连接，但只接受 HTTP(S) URL 且拒绝 userinfo；Deny 继续 fail closed。
- stdio request 写入前将连接标记为 poisoned。只有读到完整且匹配当前 request ID 的响应行后清除此标记；写入失败、取消、超时、无效帧或其他读取错误都保留 poisoned 状态。
- 被中断请求的调用方丢弃整个 stdio transport 并将客户端标为 Offline；`is_alive` 对 poisoned transport 返回 false，使既有 health monitor 可触发 reconnect。不会从残余字节继续解析新请求。

## 影响

MCP Ask/Restricted 的地址分类跟随 Common 的共享安全策略。被中断的 stdio 子进程需要重新启动，进行中的响应不会恢复；完整匹配响应（包括完整 JSON-RPC error frame）仍可复用连接。配置 schema、MCP 协议字段和持久化契约不变。

## 验证

MCP 回归覆盖 IPv4-mapped loopback 与 `100.64.0.1` 特殊地址拒绝、半帧取消和超时后的 poison、完整匹配帧恢复，以及 poisoned 连接的 liveness/discard 行为。`cargo test --locked -p haven-mcp` 全部通过。

## 回滚

不得让取消或超时后仍可能含有未完成响应前缀的 stdio 流继续服务新请求。若协议实现改为可安全复位的 framing reader，须先证明错误/取消后缓冲区和响应关联仍然完整，再调整 poisoned 状态策略。
