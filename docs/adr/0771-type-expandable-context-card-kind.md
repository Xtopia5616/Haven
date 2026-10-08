# ADR 0771：ExpandableContextCard kind 使用闭合集合

## 状态

已采纳并实施（2026-10-08）。

## 背景

`ExpandableContextCard.cardKind` 在应用内只有 `builtin-family`、`builtin-root` 与 `mcp-server` 三个静态 consumer。两个 builtin kind 驱动 card-body CSS，ToolsView 测试按 kind 查询；MCP kind 标识 card family。原 prop 类型是开放 `string`，并以空字符串代表未设置。

## 决定

1. 将 `cardKind` 收窄为三个已登记的 presentation 值。
2. 缺省保持 `undefined` 并直接交给 `data-card-kind`，由 Svelte 省略 attribute；删除空字符串哨兵。
3. 不改变现有 CSS selector、MCP/builtin rendering、expand/menu 状态或数据契约。

## 验收与回滚

UI type check 验证所有调用点；全量 UI tests 验证行为及 builtin selector。无 IPC、配置或持久数据变更；回滚 prop union 与可选值投影即可恢复开放字符串输入。
