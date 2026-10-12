# ADR 1088：保持 MCP 工具名称身份单射

## 状态

已接受（2026-10-13）

## 背景

MCP server/tool 名称由 provider 提供。旧的 `llm_tool_name` 将所有不安全字符折叠成下划线，使同一 server 下 `foo.bar` 和 `foo_bar` 生成同一个注册键。session overlay 按工具名存储，会覆盖其中一项，而 loader 仍可能报告两个外部名称都已加载。

## 决定

- MCP server 与 tool 名称的组件编码必须单射：ASCII 字母、数字和连字符原样保留；其他 UTF-8 字节编码为 `_` 加两位小写 hex，包括下划线本身。
- 组件编码后再使用公共 provider-safe 长度处理；未超长的名称保持确定性，超长名称带摘要后缀。
- 适配器、load_mcp、catalog 和权限 key 继续共用 `McpToolAdapter::qualified_name_of`，不各自重复转义。

## 影响

常见仅含字母、数字和连字符的名称不变；包含标点或下划线的 MCP 工具获得新的 provider-facing 名称。执行路由和授权仍由同一名称函数驱动。已有针对旧碰撞名称的授权 key 不迁移到新名称，相关调用会重新经过授权流程。

## 验证

- 回归测试证明 `foo.bar` 和 `foo_bar` 的 qualified name 不同且符合 provider 字符和长度约束。
- Tools 适配器测试与 workspace Rust 门禁。

## 回滚

不得恢复会把不同外部工具身份映射到同一注册键的归一化方式。
