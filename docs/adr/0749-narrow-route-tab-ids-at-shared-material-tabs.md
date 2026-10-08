# ADR 0749：路由页签在共享 MaterialTabs 边界收窄

## 状态

已采纳并实施。

## 背景

`MaterialTabs` 是通用导航 primitive，`NavigationTab.id` 与 `onNavigate` 合法使用开放字符串。ToolsView 将同一组三个 tab id 重复写在 state union、callback guard 与 options 中；SettingsView 的 tab state 和 visited 列表则直接使用 `string`，raw callback 可不经检查地写入状态。

## 决定

- 保留 `MaterialTabs` 的开放导航接口，由每个闭合集合的路由 view 在回调边界校验。
- ToolsView 用 `TOOL_TAB_IDS` tuple 生成 `ToolTabId`、guard 和有序 options，标签用穷尽 `Record<ToolTabId, string>`。
- SettingsView 的 `SettingsTabId` 从 `SETTINGS_SECTIONS` 派生；active、visited、dirty section ids 和 model/media 子页引用该类型，callback 先按 section source guard。

## 影响与验证

只收窄 UI 内部 route state；MaterialTabs 公共 props、有效 tab 顺序、URL、IPC 与持久化不变。通过 Svelte 类型检查及全量 UI 测试验证。

## 回滚

恢复各 view 的开放字符串状态和重复 id literal 即可；没有外部契约迁移。
