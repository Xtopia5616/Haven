# ADR 0002：隔离本机工具测试与未配置 OCR

日期：2026-08-26  
状态：已采纳

## 背景

MCP 配置生命周期和客户端集成测试曾依赖 PATH 中的 Python 与外部测试进程；窗口 OCR 与 UI 自动化测试则隐式要求当前进程可访问交互式 Windows 桌面。这些前提在 CI、远程会话和受限开发环境中不成立，使质量门禁无法重复。另一个问题是，当未配置视觉路由时，OCR 仍会先截取桌面，随后才报告能力不可用；这会产生无效且不必要的高风险数据采集。

## 决定

- MCP 配置生命周期和客户端集成测试在进程内启动最小 HTTP MCP 端点；前者通过真实 `McpManager`，后者通过生产 `McpClient`，验证初始化、工具发现、调用、内容映射、重载、断开和 liveness 语义。
- 窗口 UI 树单元测试只验证不存在目标时的明确失败，不依赖系统桌面上是否存在可枚举窗口。
- `window` 工具的 OCR 操作在没有视觉路由时立即返回 `ocr_unavailable`，不创建截图文件，也不访问桌面；配置了视觉路由后才执行截图与 OCR。

## 替代方案

保留 Python fixture、按环境跳过测试，或在无桌面时将所有窗口错误视为成功。前两者分别保留外部机器依赖或降低验证覆盖；后一种会掩盖真实的窗口操作失败，均被拒绝。

## 影响

测试不再要求 Python、外部 MCP 服务或交互式桌面；旧 Python fixture 已删除。未配置视觉路由的 OCR 调用仍返回成功的能力不可用结果，但不再包含 `path` 或 `screenshot` 字段；调用方应以 `ocr_unavailable` 判断降级。

## 验证

`cargo test -p haven-tools --lib -- --test-threads=1`、`cargo test -p haven-tools --test mcp_integration -- --test-threads=1`，以及完整的 `cargo test --workspace -- --test-threads=1`。Windows 环境还应覆盖无可交互桌面的测试运行。

## 回滚与重置

这是运行时工具与测试行为变更，不涉及数据库、配置格式或快照，无需重置用户数据。若回滚，恢复旧实现即可；不应恢复依赖真实用户桌面或默认工作目录的测试写入。
