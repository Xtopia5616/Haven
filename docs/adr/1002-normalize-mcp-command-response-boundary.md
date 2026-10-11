# 1002：收敛 MCP command response 校验与 renderer view

## 状态

已采纳并实施。

## 背景

`list_mcp_servers` 的 UI command adapter 只校验 MCP status 与 transport enum，其它字段仍被当作 `McpServerSnapshot` 使用；卡片和编辑器因此直接消费 Rust snake_case response。`refresh_mcp_servers` 则未经运行时校验就把四组字符串数组拼进通知。profile name 是有精确格式和大小写身份规则的配置身份，重复的 list row 会破坏 Svelte keyed list 的实体身份。

## 决定

- UI 的 `contracts/mcp.ts` 校验 list response 的完整必需形状：profile name、transport、command、args/env、nullable cwd、url、enabled、status、tools、nullable diagnostics/error 与整数 `last_seen_at`。
- MCP tool 的动态 `input_schema` 保留开放 JSON 数据，但根值必须是 JSON object；闭合 status 与 transport 继续复用生成的枚举。
- profile name 复用已有精确规则；list response 拒绝重复 name。
- generated snake_case row 在命令边界映射为 `McpServerSnapshotView` / `McpToolInfoView` camelCase，Tools 卡片与编辑器只消费 view。
- refresh response 的 added/removed/updated/failed 数组逐项按 profile name 校验，并拒绝组内重复。不同类别间允许重叠：changed server 即使重连失败，也同时属于 `updated` 与 `failed`。
- Rust 命令、生成的 IPC wire shape、授权、配置写入和 MCP 连接行为保持不变。

## 影响

畸形或不完整的 MCP command response 现在在 UI boundary 失败关闭；list renderer state 统一使用 camelCase。此切片不更改持久化 schema、配置或 Rust IPC payload，无需重置数据。未知的附加 snapshot 字段被忽略，动态 JSON Schema 内容保留。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
- 对本次改动的 UI 文件运行 Prettier 检查。
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 回滚

可恢复现有 wire DTO 直用方式，无需数据迁移；如果回退 parser，后续必须恢复完整 shape、profile identity 和 renderer boundary 校验后才能继续扩展 Tools UI。
