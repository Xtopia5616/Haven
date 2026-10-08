# ADR 0808：使用生成的 LayoutProps 类型化 route children

## 状态

已采纳并实施（2026-10-08）。

## 背景

全 UI Svelte props 复扫发现 `+layout.svelte` 是唯一未为 `$props()` 声明输入类型的组件。该 route 接收 SvelteKit 的 layout `children` snippet；SvelteKit 已为该 route 生成 `LayoutProps`，其中还准确记录 params/data 等路由属性。

## 决定

从本地 `./$types` 导入生成的 `LayoutProps`，让 layout 的 `$props()` 解构显式使用它。忽略未使用的 params/data 字段，不另写局部 children alias。

## 影响与回滚

只增加编译期 route prop 契约；layout 渲染顺序、导航和页面 slot 行为不变，无 IPC 或持久化变化。回滚只需移除类型导入和注解。

## 验收

运行 UI 类型检查、完整 UI 测试和 ADR 索引检查；类型检查验证 SvelteKit 生成的 `LayoutProps` 与本地 route 一致。
