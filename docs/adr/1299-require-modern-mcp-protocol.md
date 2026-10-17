# ADR 1299：仅支持当前无状态 MCP 协议

状态：已接受并实施（2026-10-17）。

## 背景

MCP client 同时实现 `server/discover` 和较旧 `initialize` 协商、协议 session header、旧版 HTTP GET SSE listener，以及两套 capability/result 处理。双路径增加了连接状态和测试分支，并让旧 peer 在一条配置上看似可连接，却无法保证当前工具调用和传输行为。

## 决定

1. stdio 与 Streamable HTTP 都必须通过 `server/discover` 声明当前 `2026-07-28` 协议版本。握手错误、缺少版本或未声明当前版本时连接失败，不回退到 `initialize`。
2. 所有请求使用当前逐请求 protocol/client metadata。HTTP 请求还携带协议版本、方法路由和经过校验/编码的参数路由头；不读写 `Mcp-Session-Id`。
3. 删除旧 `McpProtocolMode`、`initialize`/`notifications/initialized`、旧 capability/result 兼容和独立 HTTP GET SSE listener。HTTP server notification 仅通过 `subscriptions/listen` 的 request-scoped SSE 响应接收；普通请求仍接受 JSON 或 request-scoped SSE。
4. 保留 stdio `notifications/cancelled`、HTTP subscription cancellation、有限队列和 SSE buffer 限制、请求/响应 byte cap、deadline、HTTP 目标及 header 安全校验，以及关闭 stdin 后有界等待/终止 stdio process 的生命周期逻辑。关闭现代 stdio generation 不发送旧 `shutdown`/`exit` RPC。
5. `McpToolExposure::Legacy` 是当前工具暴露策略，仍保留；不改配置、数据库、IPC DTO 或持久数据。

## 替代方案

- 保留旧握手作为 fallback：拒绝。它延续双套状态和传输行为，且无法保证当前静态能力与现代 request metadata 契约。
- 只删 HTTP session header 或 SSE listener：拒绝。旧 initialize/capability/result 分支仍会保留另一套协议行为。
- 为旧 peer 自动探测后给出警告并继续连接：拒绝。旧 peer 无法完成当前要求的握手，应明确失败，由用户升级或替换 peer。

## 影响与恢复

- 只支持旧 MCP initialize/session 的外部 peer 将无法连接；更新 peer 到当前 `server/discover` 协议后可重新连接。UI 的 HTTP 配置说明列明支持范围。
- 本次不改变用户配置、数据库 schema、缓存持久格式或 IPC，无需备份、迁移或重置用户数据。
- 回退 Haven 二进制可恢复旧连接行为；无需数据转换。回退不会改变已保存配置。

## 验收

- 覆盖现代 stdio/HTTP 的发现、工具列表/调用、请求级 SSE、订阅取消和 bounded shutdown；旧 HTTP peer 在现代 discovery 处明确失败且不发送 initialize/session 请求。
- 运行 MCP/Tools locked tests 与 workspace locked tests/严格 Clippy；UI 配置说明变更运行 UI check、tests 和 build；运行 ADR 索引检查、格式与差异检查。
- 真实 Windows 子进程与 HTTP 生命周期行为在本环境可运行的门禁范围内记录；未执行的平台专属验收单独注明。
