# ADR 0770：Context menu 图标使用注册表键类型

## 状态

已采纳并实施（2026-10-08）。

## 背景

`ContextMenuItem.icon` 由 UI 内部 action builder 构造，仓库内生产调用点只使用 `icons.ts::ICONS` 登记的固定键，但类型为开放 `string`。共享菜单的 `ContextMenu` 已在渲染前用 `hasIcon` 跳过未登记值。

## 决定

1. `ContextMenuItem.icon` 使用 `icons.ts::IconName`，使 context-menu producer 在编译期引用注册表键。
2. 保留 `ContextMenu` 的 `hasIcon` 运行时 guard，防止通过 JS、动态数据或类型规避传入的无效值被渲染。
3. 底层 `Icon` 的动态 metadata/fallback 契约、菜单项顺序、操作和渲染行为不变。

## 验收与回滚

UI type check 验证全部 producer；全量 UI tests 验证菜单操作与呈现保持。无 IPC、配置或持久数据变更；回滚 `ContextMenuItem.icon` 的 prop 类型即可恢复开放字符串输入。
