# ADR 0792：导航页签使用注册表图标键

## 状态

已采纳并实施（2026-10-08）。

## 背景

`NavigationTab.icon` 是应用内导航 renderer 的输入，但声明为开放 `string`。两个导航 renderer 还会把任意 tab id 当作图标名传递给通用 `Icon`；未知值虽会退化为 help 图标，却让视图契约无法表达图标注册表的闭合集合。

## 决定

1. `NavigationTab.icon` 使用 `icons.ts::IconName`，由共享导航类型 owner 约束应用内页签 producer。
2. 导航 renderer 通过共享解析器使用已登记的显式图标；未提供图标时，仅当 tab id 本身是注册表键才用作回退，否则使用 `help`。无效运行时图标值同样安全回退。
3. 保留底层 `Icon.name` 的开放字符串输入与 help fallback，因为工具 manifest 图标仍是 backend-owned 动态字符串（ADR 0769）。不改变 manifest、IPC、tab identity 或已登记图标的显示。

## 影响与回滚

仅收紧 UI 内部导航 renderer props，不改变 IPC 或持久化值。新增图标时需先更新共享 registry；回滚时可恢复 `NavigationTab.icon` 的开放字符串并移除导航解析器。

## 验收

UI 类型检查确认导航 producer 使用注册表键；UI 全量测试覆盖显式图标、tab id 回退、未知 id 与非法运行时图标。无 IPC、配置或持久化变更。
