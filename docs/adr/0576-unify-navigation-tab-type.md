# ADR 0576：统一共享导航页签类型

## 状态

已采纳并实施。

## 背景

`MaterialTabs` 与 `WorkspaceNav` 各自声明 `TabItem`，`AppShell` 与 `LandscapeWorkspaceNav` 又各自声明 `WorkspaceTab`。四份结构字段完全相同：`id`、`label`、可选 `hint` 与 `icon`。`MaterialTabs` 被工作区以及工具、记忆、设置子页共用，不能由某个 workspace view 独占该结构。

## 决定

1. 在 `navigationTypes.ts` 定义共享 `NavigationTab` renderer shape。
2. `MaterialTabs`、`WorkspaceNav`、`LandscapeWorkspaceNav` 与 `AppShell` 的 props 使用此类型。
3. 页面路由可继续将 `NavigationTab.id` 收窄为该路由拥有的 tab id；共享类型不拥有路由状态。

## 替代方案

- 继续在组件内复制 `TabItem` / `WorkspaceTab`：拒绝，字段已经一致且通过父子 props 传递。
- 以 `WorkspaceTab` 作为通用名称：拒绝，MaterialTabs 也承载非 workspace 页签。
- 让通用 tabs primitive 拥有页面 tab id union：拒绝，id 集合属于各页面/路由，primitive 只呈现页签。

## 影响与验证

- 仅合并 Svelte 组件 props 的 renderer 类型；DOM、tab 选择、图标与导航行为不变。
- 无 Rust、IPC、持久化或安全契约变化。
- 验证：UI `check`、`test:run`、`build`、ADR 索引与差异空白检查。

## 回滚

移除 `NavigationTab` 并恢复四个组件中原有局部页签声明。
