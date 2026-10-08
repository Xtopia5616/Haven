# ADR 0795：类型化 MCP server snapshot transport

## 状态

已采纳并实施（2026-10-08）。

## 背景

Common 配置的 `McpTransportType` 仅允许 `stdio` / `http`，工具配置、MCP client 与 MCP `McpServerSnapshot.transport` 却再次降为 `String`。App 的 `list_mcp_servers` 直接序列化该 snapshot，generated UI DTO 因而也是开放字符串；runtime mapper 只验证 status，MCP server card 还会把缺失 transport 静默显示为 `stdio`。

## 决定

1. MCP `McpServerSnapshot.transport` 使用 Common `McpTransportType`；App 对已配置但无 live client 的 snapshot 直接复制 enum。
2. IPC generator 将 snapshot transport 生成为 `McpTransportType`，UI list mapper 验证同一 generated 值集，并拒绝未知值。
3. `McpServerCard` 直接呈现必需 transport，不再把错误缺值替换为默认 `stdio`。

## 影响与回滚

JSON 仍序列化为 `stdio` / `http`，不改变 command 名、设置表单、数据存储、MCP protocol 或凭据遮蔽行为；只是收紧 Rust 与 UI 内部 DTO 的类型边界。无数据库/配置重置。回滚需同步恢复 snapshot 字符串字段、mapper 宽松校验、renderer fallback 与 generated contract。

## 验收

MCP serde/client 测试验证 wire 值不变，UI tests 覆盖未知 transport 拒绝；运行 IPC contract generator/checker、Rust workspace 格式/Clippy/测试，以及 UI check/test/build。无配置或持久化迁移。
