# ADR 0827：区分未知 MCP 工具目录与空目录

## 状态

Accepted — 2026-10-09

## 背景

MCP `tools/list` 返回实际工具 schema。未连接的 server 尚未运行该请求，因此 `connected: false` 且 `tools: 0` 容易被误读成“已连接并确认没有工具”；无 live client 时 `last_error` 为空也没有说明这是尚未加载还是调用失败。

## 决定

- `McpClient::cached_tools_count` 表达 `None`（本次 client 尚无 tools/list 响应）与 `Some(0)`（已发现空目录）的区别。
- Admin `mcp_list` 的 `tools` 在未发现时序列化为 `null`，并提供明确诊断；配置启用但无 live client 时同样说明发现尚未运行，不伪造错误或端口拒绝结论。实际 connect 失败继续显示客户端已有错误/诊断。
- Tauri `list_mcp_servers` 无 live client 的启用服务附加相同诊断；Tools 页将未知工具数显示为“工具未探测”，真实已连接的 0 才显示“0 个工具”。
- MCP schema 未发现前不能安全注册为可调用工具：工具名和 input schema 都来自远端 tools/list。保留渐进式发现/加载；要实现真正静态注册，需另行定义并校验 config-owned schema 契约，不能把空列表或上次 schema 当作当前能力。

## 替代方案

- 将未连接服务报告为 0 个工具：拒绝。它混淆未知状态与已验证空目录。
- 在 Haven 中猜测 MCP 子进程端口探测错误：拒绝。Haven 不拥有外部 launcher 的内部健康探测，只有收到连接失败或 launcher 明确返回诊断时才能报告该结果。

## 影响与验证

Admin `mcp_list.tools` 可空，动态 ToolResult 增加发现状态诊断；`McpServerSnapshot` 的 IPC 类型不变，复用既有 `diagnostic` 字段。无配置或持久数据变化。适用门禁：Rust workspace 编译/严格 Clippy、前端类型检查与 diff 检查。测试因本次执行约束未运行。

## 回滚

恢复将缺失缓存映射为 0、清除未连接服务诊断并恢复 UI 固定显示数量。仅影响进程内状态和动态工具输出，不需要配置或数据库重置。
