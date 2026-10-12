# ADR 1084：明确命名 MCP 协议 session 身份

## 状态

已接受（2026-10-13）

## 背景

Streamable HTTP transport 的 `HttpShared.session_id` 实际保存 MCP peer 返回的 `Mcp-Session-Id`，并在后续请求中原样回发。项目内部 `session_id` 专指 Haven 对话；共用这个名称会让协议身份与本地对话身份发生混淆。

## 决定

- transport 内部共享字段、header 读取局部变量和附加 session header 的 helper 使用 `mcp_session_id` / `apply_http_mcp_session_headers`。
- HTTP header 名称和值格式保持 MCP 协议原样，不按 Haven canonical entity ID 转换。

## 影响

只改变 MCP transport 的 Rust 内部名称，不改变 wire protocol、配置或持久化数据。

## 验证

- `cargo test --locked -p haven-mcp`。

## 回滚

若未来 MCP transport 改为无状态传输，删除对应字段与 helper；恢复 `session_id` 这个含混内部名不符合项目对话身份约定。
