# ADR 0087：工作区 Tab 切换动效

## 背景

工作区一级导航采用 keep-alive：首次打开的页面保持挂载，切换时通过 `hidden` 控制可见性。这样可以保留表单草稿、滚动位置和已加载数据，但当前只有颜色和静态短线变化，用户在切换或等待懒加载时缺少即时反馈。

## 决定

- 顶部工作区导航使用一个公共活动短线，通过测量当前 Tab 的位置，以标准曲线平移到新 Tab；页面内复用的 `.md-tab` 短线保留同一原语，并以短暂缩放淡入反馈选中变化。
- 当前工作区内容的可见 surface 在每次切换时以 `opacity + translateY(6px)` 轻量入场，不使用缩放，不改变布局尺寸，不销毁 keep-alive 页面。
- 懒加载等待继续使用现有 `LoadingState` / Haven 语音柱；加载态不套 transform，避免破坏其 fixed workspace 覆盖层定位。
- `prefers-reduced-motion: reduce` 下禁用 Tab 平移、淡入和位移，只保留颜色、可见性和加载状态。

## 替代方案

- 不加动效：切换反馈仍然过弱，不采用。
- 使用整页淡出/淡入或横向滑屏：会让工作区切换显得拖沓，且容易造成内容跳动，不采用。
- 用 Svelte `key` 重建页面：会丢失 keep-alive 页面状态，不采用。

## 影响与回滚

本次只改变前端呈现，不改变路由、IPC、持久化或业务状态。回滚本提交即可移除动效；不需要删除或重置用户数据。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
