# ADR 0769：图标按钮使用注册表键类型

## 状态

已采纳并实施（2026-10-08）。

## 背景

`MaterialIconButton.icon` 只由应用内 renderer 传递图标注册表键或固定 UI 分支值，却声明为开放 `string`。底层 `Icon` 也接受工具 presentation 与导航元数据中的动态图标名称；它对未登记值使用 help 图标，因此有意保留开放入口。

## 决定

1. `MaterialIconButton.icon` 复用 `icons.ts::IconName`，由 `ICONS` 键集合定义闭合输入。所有按钮调用点须传递已登记的图标。
2. 保留低层 `Icon.name` 的开放字符串行为和未知值 fallback；动态工具/导航 metadata 不在按钮 props 的类型收窄范围内。
3. 不改变输出图像、图标 registry、工具 metadata、IPC 或 UI 行为。

## 验收与回滚

UI type check 验证全部 renderer 调用点符合注册表；全量 UI tests 保持行为。无 IPC、配置、持久数据变更；回滚 `MaterialIconButton.icon` 的 prop 类型即可恢复原宽泛输入。
