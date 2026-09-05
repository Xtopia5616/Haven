# ADR 0084：UI 语义化按钮迁移

## 背景

前端已经有 `MaterialButton` 和 `MaterialIconButton`，但工具、任务、记忆、设置和异步状态中仍有大量重复的 `md-btn` DOM。相同的按钮样式由页面自行维护，导致按钮点击行为、尺寸、危险色和响应式选择器容易发生漂移。

## 决定

- 将行为明确的文字按钮迁移到 `MaterialButton`，按语义选择 `filled`、`tonal`、`outlined`、`text` 和 `danger` 变体。
- 将小尺寸动作通过 `className="md-btn--xs"` 复用，不新增一套小按钮组件。
- `MaterialButton` 透传 `aria-expanded` 和可选 `role`，使折叠控制等特殊语义在复用后仍然完整。
- 纯图标按钮继续使用 `MaterialIconButton`；Tab、列表行、主题/强调色选择、日历格、数字步进器、输入组件内部按钮和安全确认按钮继续由领域组件持有。
- 迁移到子组件的页面样式通过 `:global(.md-btn)` 或明确的 className 选择器适配，避免 Svelte 样式作用域使响应式布局失效。

## 替代方案

- 继续直接复用全局 `.md-btn` 类：只能复用 CSS，无法统一按钮组件的 `type`、事件冒泡和属性契约。
- 用一个新组件替换所有 `<button>`：会误伤 Tab、列表行、日历控件和带自定义键盘语义的交互。
- 为每种页面再建一套按钮组件：会重新引入按钮几何和状态层的多套实现。

## 影响与回滚

本次只改变前端按钮的组合方式和语义属性，不改变业务回调、IPC、数据或任务生命周期，不需要重置。回滚本提交即可恢复迁移前的页面按钮实现。

## 验证

- `cd ui; corepack pnpm run check`
- `cd ui; corepack pnpm run test:run`
- `cd ui; corepack pnpm run build`
